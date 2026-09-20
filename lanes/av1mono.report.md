# lane-av1-mono — monochrome AV1 decode is luma byte-exact (60f kf, 120f inter, mono.ivf)

Base: `main` **7f4e8301** (the WIP `av1: WIP monochrome plane model` commit; its
parent is `f0727a87`). Worktree `edith_codecs-av1mono`, branch `lane-av1-mono`,
`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1mono` (private). No push.
Status: **COMPLETE** — a monochrome AV1 stream decodes and its luma is byte-exact
against ffmpeg's own decode, on all three witnesses. The luma half of the pilot's
first real-stream wall (§3.1 of `lanes/av1decode.report.md`) is closed.

## TL;DR

The previous continuation (Yunus) died mid-flight on a provider outage, leaving a
working tree that *was* the fix but was buried under **24,000+ lines of omp
edit-tool rustfmt churn** (`git diff --stat` read 17300/6908 changed lines). This
session (1) salvaged a **semantic-only 19-hunk diff** out of that churn, (2)
verified it fixes the luma defect, (3) proved the three witnesses, the corpus, the
film windows and the suite.

- **Salvage:** churn → `decode.rs` 88 lines, `stream.rs` 142 lines (211 insertions,
  19 deletions). Method and proof below.
- **Root cause of the luma mismatch:** a **palette-mode neighbour band** that a
  mono sub-8x8 leaf left uncleared. The publish (`record_strip_palette`) sat
  *after* the `if !has_chroma { return }` early-out; on a monochrome frame EVERY
  sub-8 leaf takes that early-out, so the band kept an earlier palette block's
  size, the next block read `palette_y_mode` off CDF row 1 where libaom reads row
  0, and the tile silently desynced one block later. (Symptom had been read as a
  reconstruction defect; it is an entropy desync.)
- **Second class closed:** the rect-strip paths call `read_coeffs_rect` on planes
  1/2 **directly** (bypassing `read_plane`'s own mono gate), so a mono frame
  consumed a chroma coefficient symbol libaom never coded. New
  `read_chroma_coeffs_rect` gates those 8 call sites.

## Salvage (what the dirty tree contained, what was kept/reverted)

`git status` at handover: `M crates/ec-av1/src/decode.rs`, `M crates/ec-av1/src/stream.rs`.

1. **Hunk classifier first** (`rustfmt-semantic-only-diff` recipe): a hunk is
   fmt-only iff its removed/added lines are equal after JOINING + stripping
   whitespace + folding trailing commas. Result: 677 hunks decode (609 fmt-only),
   426 hunks stream (406 fmt-only) — *but this classifier is incomplete here*:
   the local rustfmt **wraps `ok_or_else(|| expr)` closure bodies in `{ ... }`**,
   so 40+ pure reflow hunks were flagged "semantic" (a brace pair my normalizer
   cannot fold away). Trusting it would have kept thousands of churn lines.
2. **Decisive method instead:** rustfmt the *committed* file and diff it against
   the working tree. `rustfmt --edition 2024 <7f4e8301 file>` ≈ the working tree:
   decode.rs 43620 vs 43672 lines (52-line residual), stream.rs 38666 vs 38805
   (139-line residual). So the working tree **was** `rustfmt(HEAD) + a small
   semantic delta` — the previous agent had not left a half-applied edit.
3. Extract `S = diff(rustfmt(7f4e8301), W)` (19 + 2 hunks), apply `S` to the
   committed file, and prove correctness mechanically: **`rustfmt(HEAD + S) == W`
   byte-for-byte** on both files. Whitespace-only home for every non-semantic line
   is then guaranteed: applying only `S` to `HEAD` cannot carry churn.
4. The `patch` of `S` onto `HEAD` needed fuzz at 5 of the 8 call sites and put
   `fctx,`/`let (` at the wrong indent there; fixed by hand (indentation is
   HEAD-style; the diff now shows ONLY identifier renames, `fctx` args, the new
   fn, the palette move, and the test).
5. **Coherence:** the semantic set is coherent — no orphaned variable, no broken
   syntax, one self-contained feature (`read_chroma_coeffs_rect` + the palette
   move). Nothing had to be reverted.

## Root cause

`decode_intra_sub8_leaf` (`decode.rs`). libaom's MI grid writes
`palette_size[0] == 0` for **every** cell of a sub-8x8 group whatever the plane
count (`av1_allow_palette` needs `bsize >= BLOCK_8X8`, so no sub-8 piece is ever
a palette block). ec-av1 did publish that clear — but only on the **chroma-reference**
path, after `if !has_chroma { return Ok(mode) }`. On a mono frame
`has_chroma == false` for every sub-8 leaf (`NumPlanes == 1`), so the publish
never ran: the row's band kept an earlier palette block's size, the next intra
block read `av1_get_palette_mode_ctx` one too high, decoded a `palette_y_mode`
symbol libaom never coded, and the tile desynced.

Measured: gray testsrc2 320x240, **decode-order frame 33**, mi(42,50) — ours ctx 2
where libaom gathers 1. (First *pixel* divergence on the 60-frame stream lands at
frame 0, because the key frame's own early blocks already carry the mismatch.)

## Fix

- `crates/ec-av1/src/decode.rs`
  - **`record_strip_palette(...)` moved BEFORE the `!has_chroma` return** in
    `decode_intra_sub8_leaf` — unconditional, matching libaom's MI grid, with the
    extended comment. (This is the luma fix.)
  - New **`read_chroma_coeffs_rect`**: a mono early-return of `Grid::Zero` +
    `TxType::DctDct` before delegating to `read_coeffs_rect`; applied at the 8
    chroma call sites that read planes 1/2 directly
    (`decode_block_rect`, `decode_leaf_rect`, `decode_block_rect4`,
    `decode_rect4_16_strip`, `decode_block_rect64`). Closes the
    `reader-gate-not-on-every-path` class.
- `crates/ec-av1/src/stream.rs`
  - New witness test **`a_real_libaom_monochrome_key_frame_decodes_pixel_exact`**
    (the inverted form of the pilot's pinning gate) + `ffmpeg_decode_gray_sequence`
    helper.

## Semantic-only diff vs 7f4e8301

```diff
diff --git a/crates/ec-av1/src/decode.rs b/crates/ec-av1/src/decode.rs
index 6399854a..75cd8aad 100644
--- a/crates/ec-av1/src/decode.rs
+++ b/crates/ec-av1/src/decode.rs
@@ -6805,6 +6805,39 @@ fn read_coeffs_rect(
     Ok((Grid::Own(grid), tx_type))
 }
 
+/// [`read_coeffs_rect`] for a CHROMA plane, carrying the monochrome gate the
+/// [`read_plane`] family already has.
+///
+/// A monochrome frame has `NumPlanes == 1`, so libaom's coefficient loop
+/// (`for (plane = 0; plane < av1_num_planes(cm); ++plane)`) never reaches a
+/// chroma plane: no u/v coefficient symbol is coded, and consuming one moves
+/// the arithmetic decoder off the stream for the rest of the tile. [`read_plane`]
+/// enforces that for every strip that reads its chroma through it, but the rect
+/// strip paths ([`decode_block_rect`], [`decode_leaf_rect`],
+/// [`decode_block_rect4`], [`decode_rect4_16_strip`], [`decode_block_rect64`])
+/// call `read_coeffs_rect` on planes 1/2 DIRECTLY, so each of those sites takes
+/// this gate instead (class `reader-gate-not-on-every-path`). One un-gated
+/// chroma read desyncs at the FIRST rect leaf of the frame and every later
+/// symbol -- and therefore every later pixel -- is decoded from the wrong
+/// place, without ever tripping a refusal (the desynced values stay inside
+/// every coded alphabet).
+fn read_chroma_coeffs_rect(
+    dec: &mut SymbolDecoder,
+    coding: &mut TxbTables,
+    scan: &[u16],
+    w: usize,
+    h: usize,
+    skip_ctx: usize,
+    sign_ctx: usize,
+    default_tx_type: TxType,
+    fctx: &crate::decode::FrameCtx,
+) -> Result<(Grid, TxType)> {
+    if mono(fctx) {
+        return Ok((Grid::Zero(w * h), TxType::DctDct));
+    }
+    read_coeffs_rect(dec, coding, scan, w, h, skip_ctx, sign_ctx, default_tx_type)
+}
+
 /// What one coded block leaves behind for the blocks that read it as a
 /// neighbour: whether it coded anything at all, and the sign of its DC —
 /// [`crate::tile`]'s own private `Neighbour`.
@@ -11106,7 +11139,7 @@ fn decode_block_rect(
         let u_default_tx = default_intra_tx_type(uv_predict_mode as u8);
         let u_skip_ctx = usize::from(around[1].0) + usize::from(around[1].1);
         let mut u_coding = cdfs.txb(TxbSet::ChromaRect16x8, uv_predict_mode);
-        let (u_levels, u_tx_type) = read_coeffs_rect(
+        let (u_levels, u_tx_type) = read_chroma_coeffs_rect(
             dec,
             &mut u_coding,
             chroma_scan,
@@ -11115,6 +11148,7 @@ fn decode_block_rect(
             u_skip_ctx,
             dc_sign_ctx(around[1].2),
             u_default_tx,
+            fctx,
         )?;
         if crate::envflags::env_flag!("EC_AV1_TRACE") {
             let (rng, _) = dec.debug_state();
@@ -11149,7 +11183,7 @@ fn decode_block_rect(
         let v_default_tx = default_intra_tx_type(uv_predict_mode as u8);
         let v_skip_ctx = usize::from(around[2].0) + usize::from(around[2].1);
         let mut v_coding = cdfs.txb(TxbSet::ChromaRect16x8, uv_predict_mode);
-        let (v_levels, v_tx_type) = read_coeffs_rect(
+        let (v_levels, v_tx_type) = read_chroma_coeffs_rect(
             dec,
             &mut v_coding,
             chroma_scan,
@@ -11158,6 +11192,7 @@ fn decode_block_rect(
             v_skip_ctx,
             dc_sign_ctx(around[2].2),
             v_default_tx,
+            fctx,
         )?;
         if crate::envflags::env_flag!("EC_AV1_TRACE") {
             let (rng, _) = dec.debug_state();
@@ -11488,9 +11523,9 @@ fn decode_leaf_rect(
         let u_default_tx = default_intra_tx_type(uv_predict_mode as u8);
         let u_skip_ctx = usize::from(around[1].0) + usize::from(around[1].1);
         let mut u_coding = cdfs.txb(TxbSet::ChromaRect8x4, uv_predict_mode);
-        let (u_l, u_tx_type) = read_coeffs_rect(
+        let (u_l, u_tx_type) = read_chroma_coeffs_rect(
             dec, &mut u_coding, chroma_scan, chroma_w, chroma_h, u_skip_ctx,
-            dc_sign_ctx(around[1].2), u_default_tx,
+            dc_sign_ctx(around[1].2), u_default_tx, fctx,
         )?;
         u_levels = u_l;
         let u_residual = dequant_and_inverse_typed_wh(
@@ -11507,9 +11542,9 @@ fn decode_leaf_rect(
         let v_default_tx = default_intra_tx_type(uv_predict_mode as u8);
         let v_skip_ctx = usize::from(around[2].0) + usize::from(around[2].1);
         let mut v_coding = cdfs.txb(TxbSet::ChromaRect8x4, uv_predict_mode);
-        let (v_l, v_tx_type) = read_coeffs_rect(
+        let (v_l, v_tx_type) = read_chroma_coeffs_rect(
             dec, &mut v_coding, chroma_scan, chroma_w, chroma_h, v_skip_ctx,
-            dc_sign_ctx(around[2].2), v_default_tx,
+            dc_sign_ctx(around[2].2), v_default_tx, fctx,
         )?;
         v_levels = v_l;
         let v_residual = dequant_and_inverse_typed_wh(
@@ -11838,7 +11873,7 @@ fn decode_block_rect4(
             let default_tx = default_intra_tx_type(uv_predict_mode as u8);
             let skip_ctx = usize::from(around[plane].0) + usize::from(around[plane].1);
             let mut coding = cdfs.txb(TxbSet::Chroma8, uv_predict_mode);
-            let (levels, tx_type) = read_coeffs_rect(
+            let (levels, tx_type) = read_chroma_coeffs_rect(
                 dec,
                 &mut coding,
                 chroma_scan,
@@ -11847,6 +11882,7 @@ fn decode_block_rect4(
                 skip_ctx,
                 dc_sign_ctx(around[plane].2),
                 default_tx,
+                fctx,
             )?;
             planes.push((levels, tx_type));
         }
@@ -12488,7 +12524,7 @@ fn decode_rect4_16_strip(
                     let default_tx = default_intra_tx_type(uv_predict_mode as u8);
                     let skip_ctx = usize::from(around[plane].0) + usize::from(around[plane].1);
                     let mut coding = cdfs.txb(TxbSet::ChromaRect8x4, uv_predict_mode);
-                    let (levels, tx_type) = read_coeffs_rect(
+                    let (levels, tx_type) = read_chroma_coeffs_rect(
                         dec,
                         &mut coding,
                         chroma_scan,
@@ -12497,6 +12533,7 @@ fn decode_rect4_16_strip(
                         skip_ctx,
                         dc_sign_ctx(around[plane].2),
                         default_tx,
+                        fctx,
                     )?;
                     planes.push((levels, tx_type));
                 }
@@ -13012,7 +13049,7 @@ fn decode_block_rect64(
             let (rng, _) = dec.debug_state();
             eprintln!("EC_COEFF plane=1 row={mi_r} col={mi_c} tx_size=rect32x16 rng={rng}");
         }
-        let (u_levels, u_tx_type) = read_coeffs_rect(
+        let (u_levels, u_tx_type) = read_chroma_coeffs_rect(
             dec,
             &mut u_coding,
             chroma_scan,
@@ -13021,6 +13058,7 @@ fn decode_block_rect64(
             u_skip_ctx,
             dc_sign_ctx(around[1].2),
             TxType::DctDct,
+            fctx,
         )?;
         if coeff_trace_on() {
             let (rng, _) = dec.debug_state();
@@ -13076,7 +13114,7 @@ fn decode_block_rect64(
             let (rng, _) = dec.debug_state();
             eprintln!("EC_COEFF plane=2 row={mi_r} col={mi_c} tx_size=rect32x16 rng={rng}");
         }
-        let (v_levels, v_tx_type) = read_coeffs_rect(
+        let (v_levels, v_tx_type) = read_chroma_coeffs_rect(
             dec,
             &mut v_coding,
             chroma_scan,
@@ -13085,6 +13123,7 @@ fn decode_block_rect64(
             v_skip_ctx,
             dc_sign_ctx(around[2].2),
             TxType::DctDct,
+            fctx,
         )?;
         if coeff_trace_on() {
             let (rng, _) = dec.debug_state();
@@ -31604,6 +31643,26 @@ fn decode_intra_sub8_leaf(
             h.set(s);
         });
     }
+    // lane-t900 r23 (class [[new-map-ignores-tile-edge]]: a neighbour map
+    // needs EVERY writer): a sub-8x8 group published no palette state, so
+    // once an 8x8 leaf of the same row can BE a palette block, the group
+    // left the row's band holding that earlier block's size and the next
+    // intra block read `av1_get_palette_mode_ctx` one too high (measured:
+    // mi(30,30) of decode-order frame 1, ours ctx 1 where libaom gathers
+    // 0). No sub-8x8 piece can itself be a palette (`av1_allow_palette`
+    // needs `bsize >= BLOCK_8X8`), so the group always clears.
+    //
+    // lane-av1mono: this publish is NOT chroma work and must run BEFORE the
+    // `!has_chroma` return below. libaom's mi grid holds
+    // `palette_size[0] == 0` in every cell of such a group whatever the
+    // plane count, and on a monochrome frame EVERY sub-8 leaf passes
+    // `has_chroma == false`, so a publish placed after the return never ran
+    // at all: the band kept an earlier palette block's size, the next block
+    // read `palette_y_mode` off CDF row 1 where libaom reads row 0, decoded
+    // a palette libaom never coded, and the tile desynced one block later
+    // (measured: gray testsrc2 320x240, decode-order frame 33, mi(42,50),
+    // ours ctx 2 where libaom gathers 1).
+    record_strip_palette(neighbours, (gr, gc), 8, 8, 0, [0u16; 8], 0, [0u16; 8]);
     if !has_chroma {
         return Ok(mode);
     }
@@ -31622,15 +31681,6 @@ fn decode_intra_sub8_leaf(
     neighbours.above_uv_mode[c] = uv_predict_mode;
     neighbours.left_uv_mode[r] = uv_predict_mode;
     neighbours.record_uv_mode_mi(gr, gc, 2, 2, uv_predict_mode);
-    // lane-t900 r23 (class [[new-map-ignores-tile-edge]]: a neighbour map
-    // needs EVERY writer): a sub-8x8 group published no palette state, so
-    // once an 8x8 leaf of the same row can BE a palette block, the group
-    // left the row's band holding that earlier block's size and the next
-    // intra block read `av1_get_palette_mode_ctx` one too high (measured:
-    // mi(30,30) of decode-order frame 1, ours ctx 1 where libaom gathers
-    // 0). No sub-8x8 piece can itself be a palette (`av1_allow_palette`
-    // needs `bsize >= BLOCK_8X8`), so the group always clears.
-    record_strip_palette(neighbours, (gr, gc), 8, 8, 0, [0u16; 8], 0, [0u16; 8]);
 
     let ac = alpha.map(|_| cfl_src(gpx, gpy, 8));
     let (u_grid, v_grid): (Grid, Grid) = if skip {
diff --git a/crates/ec-av1/src/stream.rs b/crates/ec-av1/src/stream.rs
index a0b1bd43..d361662c 100644
--- a/crates/ec-av1/src/stream.rs
+++ b/crates/ec-av1/src/stream.rs
@@ -2024,6 +2024,105 @@ pub(crate) mod tests {
         );
     }
 
+    /// lane-av1mono: the INVERSE of the pilot's
+    /// `a_real_libaom_monochrome_key_frame_is_refused_by_name`, which the
+    /// pilot's merge `d71084ae` landed on `main` (after this lane's base
+    /// `7f4e8301`): the very same recipe now decodes, and its luma is
+    /// byte-exact against ffmpeg's own decode of the same OBU. A merge of this
+    /// lane into `main` must therefore resolve that pinning gate by adopting
+    /// this witness in its place.
+    ///
+    /// `ffmpeg -pix_fmt gray -c:v libaom-av1` sets `color_config.mono_chrome`,
+    /// so libaom codes NO chroma symbol anywhere in the sequence -- not the
+    /// mode syntax, not a coefficient. That reaches all three classes this lane
+    /// had to close: chroma SYMBOL reads (`uv_mode`/cfl/angle/palette), chroma
+    /// COEFFICIENT reads on the rect-strip paths that call `read_coeffs_rect`
+    /// DIRECTLY instead of through `read_plane`'s own plane gate, and the
+    /// palette-mode neighbour band a mono sub-8x8 leaf must still clear before
+    /// it returns (an uncleared band made the next block read `palette_y_mode`
+    /// off the wrong CDF row and silently desynced the rest of the tile --
+    /// measured at decode-order frame 33 of this very recipe).
+    ///
+    /// The 4:2:0 twin of the same source and settings is the control: it proves
+    /// the recipe is decodable, so a failure here is about the plane count and
+    /// not about the source or the encoder settings.
+    #[test]
+    fn a_real_libaom_monochrome_key_frame_decodes_pixel_exact() {
+        const NAME: &str = "a_real_libaom_monochrome_key_frame_decodes_pixel_exact";
+        if !have_ffmpeg() {
+            eprintln!("SKIP {NAME}: no ffmpeg");
+            return;
+        }
+        let emit = |pix: &str| -> Vec<u8> {
+            let out = Command::new("ffmpeg")
+                .args([
+                    "-v",
+                    "error",
+                    "-f",
+                    "lavfi",
+                    "-i",
+                    "testsrc2=size=320x240:rate=30:duration=2",
+                    "-pix_fmt",
+                    pix,
+                    "-c:v",
+                    "libaom-av1",
+                    "-cpu-used",
+                    "8",
+                    "-b:v",
+                    "300k",
+                    "-f",
+                    "obu",
+                    "-",
+                ])
+                .stdin(Stdio::null())
+                .stdout(Stdio::piped())
+                .stderr(Stdio::piped())
+                .output()
+                .expect("ffmpeg failed to run");
+            assert!(
+                out.status.success(),
+                "{NAME}: ffmpeg ({pix}) failed: {}",
+                String::from_utf8_lossy(&out.stderr)
+            );
+            out.stdout
+        };
+        // CONTROL: the identical recipe at 4:2:0 still decodes.
+        let twin = decode_stream(&emit("yuv420p")).expect("the 4:2:0 twin must decode");
+        assert!(!twin.is_empty(), "{NAME}: the 4:2:0 twin decoded no frames");
+        // THE WITNESS: the monochrome stream decodes and every luma sample
+        // equals ffmpeg's own decode of the same OBU.
+        let gray = emit("gray");
+        let ours = decode_stream(&gray)
+            .unwrap_or_else(|e| panic!("{NAME}: the monochrome stream is REFUSED again: {e}"));
+        let (width, height) = (320usize, 240usize);
+        const FRAMES: usize = 60;
+        assert_eq!(ours.len(), FRAMES, "{NAME}: expected {FRAMES} mono frames");
+        let reference = ffmpeg_decode_gray_sequence(&gray, width, height, FRAMES);
+        for (i, plane) in reference.chunks_exact(width * height).enumerate() {
+            assert_eq!(ours[i].width, width, "{NAME}: frame {i} width");
+            assert_eq!(ours[i].height, height, "{NAME}: frame {i} height");
+            assert_eq!(
+                ours[i].y.len(),
+                width * height,
+                "{NAME}: frame {i} luma length"
+            );
+            for (j, (got, want)) in plane.iter().zip(ours[i].y.iter()).enumerate() {
+                assert_eq!(
+                    *want,
+                    u16::from(*got),
+                    "{NAME}: frame {i} luma sample {j} (row {}, col {}) is {want}, ffmpeg says {got}",
+                    j / width,
+                    j % width
+                );
+            }
+        }
+        eprintln!(
+            "{NAME}: 4:2:0 twin {} frames, monochrome {} frames byte-exact vs ffmpeg",
+            twin.len(),
+            ours.len()
+        );
+    }
+
     /// lane-t900 r25: the refusal "a frame OBU with no tile group" names a
     /// shape the parser cannot hand [`decode_stream`].
     ///
@@ -3296,6 +3395,49 @@ pub(crate) mod tests {
             .collect()
     }
 
+    /// As [`ffmpeg_decode_sequence`], for a MONOCHROME stream: one 8-bit luma
+    /// plane per frame, exactly what `ffmpeg -pix_fmt gray -f rawvideo` writes
+    /// (lane-av1mono). Fed down a stdin writer thread for the reason the 4:2:0
+    /// helper documents.
+    fn ffmpeg_decode_gray_sequence(
+        stream: &[u8],
+        width: usize,
+        height: usize,
+        frames: usize,
+    ) -> Vec<u8> {
+        let mut child = Command::new("ffmpeg")
+            .args([
+                "-v", "error", "-f", "obu", "-i", "-", "-f", "rawvideo", "-pix_fmt", "gray", "-",
+            ])
+            .stdin(Stdio::piped())
+            .stdout(Stdio::piped())
+            .stderr(Stdio::piped())
+            .spawn()
+            .expect("ffmpeg failed to start");
+        let mut stdin = child.stdin.take().expect("ffmpeg stdin");
+        let payload = stream.to_vec();
+        let writer = std::thread::Builder::new()
+            .name("ec-av1-ffmpeg-gray-in".into())
+            .spawn(move || {
+                let _ = stdin.write_all(&payload);
+            })
+            .expect("spawning the ffmpeg stdin writer");
+        let out = child.wait_with_output().expect("ffmpeg failed to run");
+        writer.join().expect("ffmpeg stdin writer thread");
+        assert!(
+            out.status.success(),
+            "ffmpeg refused the monochrome stream: {}",
+            String::from_utf8_lossy(&out.stderr)
+        );
+        assert_eq!(
+            out.stdout.len(),
+            width * height * frames,
+            "expected {frames} monochrome frames, ffmpeg said: {}",
+            String::from_utf8_lossy(&out.stderr)
+        );
+        out.stdout
+    }
+
     /// As [`ffmpeg_decode_sequence`], but for a `yuv420p10le` stream: samples
     /// are 2-byte little-endian, one full 16-bit range regardless of the
     /// stream's real 10-bit depth (ffmpeg's rawvideo muxer never packs to the
```

## Witnesses (quoted)

All three run on the final tree; the probe is
`$HOME/.cache/cargo-target-av1mono/release/examples/decode_probe`.

```
(a) gray 60f key frame -- ffmpeg testsrc2 320x240 30fps 2s -pix_fmt gray
OK: 60 frames decoded, 320x240
gray60 EQUAL True

(b) gray 120f (inter frames) -- same recipe, duration 4
OK: 120 frames decoded, 320x240
gray120 EQUAL True

(c) fixtures/bitstreams/av1-monochrome.ivf (remuxed to OBU)
OK: 60 frames decoded, 320x240
mono.ivf EQUAL True
```

Comparison for (a)/(b)/(c): probe `EC_PROBE_OUT16` writes the 8-bit sample in the
**low byte** of each u16, so the check is `probe[0::2] == ffmpeg -pix_fmt gray -f
rawvideo`. All three are STRONGER than the charter expected: (c) was expected to
stop at the named intrabc-rect refusal, but that refusal was itself a *symptom* of
this desync, so with the desync gone the fixture now decodes **all 60 frames
byte-exact**. (Class, per the pilot §3.3/3.4: a routing refusal reached only
because the desynced read picked wrong block sizes.)

Scoped test:
```
$ cargo test -p ec-av1 --lib a_real_libaom_monochrome_key_frame_decodes_pixel_exact -- --nocapture
a_real_libaom_monochrome_key_frame_decodes_pixel_exact: 4:2:0 twin 60 frames, monochrome 60 frames byte-exact vs ffmpeg
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 651 filtered out
```

## Regression

**Corpus (9 `fixtures/bitstreams/av1-*.ivf`), byte-identity vs the LANE BASE
(`7f4e8301`, own probe at `$HOME/.cache/cargo-target-av1base`):**

```
av1-1080p-23.976-10bit IDENTICAL(rc=0) da2b8484b94b244a
av1-1080p-23.976-8bit  IDENTICAL(rc=0) 407148a9397461fa
av1-1080p-60-10bit     IDENTICAL(rc=0) 83087317d215b128
av1-1080p-60-8bit      IDENTICAL(rc=0) 4aff1fdedc0599a8
av1-2160p-23.976-8bit  IDENTICAL(rc=0) ea683b4a9e2b8891
av1-altref             IDENTICAL(rc=0) ea2f84d929b52f5e
av1-monochrome         DIFFER base=49d757b50b8d9175 lane=b869441a9cf29d87  <-- the fix
av1-profile1-444       IDENTICAL(rc=0) 6620e894ff971cba
av1-tiles-1280         IDENTICAL(rc=0) 5e59dbc445eb4e17
```

Only the monochrome fixture moves; every non-mono stream is bit-identical to the
base. **`av1-profile1-444` is IDENTICAL to the base and is NOT byte-exact vs
ffmpeg** — that is the pilot §3.2 4:4:4 block-routing defect, pre-existing and
out of this lane's scope (same probe, same output at base and lane).

**Against ffmpeg** (correct sample packing per bit depth):
`av1-1080p-23.976-10bit`, `av1-1080p-23.976-8bit`, `av1-1080p-60-10bit`,
`av1-1080p-60-8bit`, `av1-2160p-23.976-8bit`, `av1-altref`, `av1-monochrome`,
`av1-tiles-1280` → all MATCH (sha256). `av1-profile1-444` → differs (above).

**Film windows** (the pilot's 13 real 3840x1608 10-bit windows
`~/.cache/av1dec-recon/p-*.obu`, compared to the pilot's own ffmpeg `.ref` decodes):

```
p-hg2700 MATCH [OK: 104 frames] p-hg300 MATCH [OK: 136] p-hg5700 MATCH [OK: 247]
p-hgb1500 MATCH [OK: 242] p-hgb2100 MATCH [OK: 173] p-hgb3300 MATCH [OK: 279]
p-hgb3900 MATCH [OK: 210] p-hgb4500 MATCH [OK: 141] p-hgb5100 MATCH [OK: 71]
p-hgb6300 MATCH [OK: 177] p-hgb6900 MATCH [OK: 108] p-hgb7500 MATCH [OK: 284]
p-hgb900  MATCH [OK: 67]
```

All 13 byte-identical. Refusals were not touched (`refusal_inventory` tests are in
the suite below).

## Merge interaction (for the orchestrator)

`main` now carries the pilot's **`a_real_libaom_monochrome_key_frame_is_refused_by_name`**
(merged at `d71084ae`, after this lane's base `7f4e8301`). This lane adds the
**inverted witness** `..._decodes_pixel_exact` in the same `stream.rs` test module
and does NOT edit the pinning test (per charter). A merge will **conflict in
`crates/ec-av1/src/stream.rs`**: the resolution is to DROP the pilot's pinning
gate and keep this lane's byte-exact witness (the pinning gate asserts the very
behaviour this lane removes, so it must fail once mono decode lands). The
witness test's own doc comment records this.

## Gates

- `cargo check -p ec-av1` (post-salvage) → rc 0.
- `cargo check -p ec-av1 --all-targets` → rc 0, **0 warnings** (second target dir
  `$HOME/.cache/cargo-target-av1mono-check`).
- `cargo test -p ec-av1 --lib` to completion → **592 passed, 0 failed, 60 ignored**
  (6724 s), on the final tree.

## Repro

```
ffmpeg -v error -f lavfi -i "testsrc2=size=320x240:rate=30:duration=2" \
  -pix_fmt gray -c:v libaom-av1 -cpu-used 8 -b:v 300k -f obu -y /tmp/gray.obu
P=$HOME/.cache/cargo-target-av1mono/release/examples/decode_probe
EC_PROBE_OUT16=$HOME/m.raw16 $P /tmp/gray.obu            # OK: 60 frames
ffmpeg -v error -i /tmp/gray.obu -pix_fmt gray -f rawvideo $HOME/ref.gray
python3 -c "d=open('$HOME/m.raw16','rb').read()[0::2]; print(bytes(d)==open('$HOME/ref.gray','rb').read())"  # True
```

## Suite

```
$ cargo test -p ec-av1 --lib
test result: ok. 592 passed; 0 failed; 60 ignored; 0 measured; 0 filtered out; finished in 6724.24s
```

Same count as the pilot's completed run on `main` (592/0/60, 6716 s), so no test
regressed, no test was added-but-unrun, and the 60 ignored are the pre-existing
single-process-global cases. `cargo check -p ec-av1 --all-targets` → rc 0, 0
warnings on the same final bytes.

## Deferred / not chased

- `av1-profile1-444.ivf` chroma non-exactness (pilot §3.2 4:4:4 block-routing) —
  pre-existing, unchanged by this lane.
- The pilot's 4:4:4-fast / all-intra / warped-cpu0 recipes — untouched.

## MERGE-SIDE FOLLOW-UPS

Merged `lane-av1-mono` (`66a39803`) into `main` as the merge commit
**`7806fd0d`** (parents `0cfb9af5` + `66a39803`; a real merge, not a
fast-forward). Pushed to `origin/main`.

### Conflict resolution

One conflicted file: `crates/ec-av1/src/stream.rs`, three hunks, all inside
the pilot's pinning gate vs this lane's witness.

- **DROP the pilot's pinning gate**
  `a_real_libaom_monochrome_key_frame_is_refused_by_name` (landed on `main` at
  the pilot merge `d71084ae`, after this lane's base `7f4e8301`). It asserts the
  exact refusal this lane removes, so it cannot survive.
- **KEEP this lane's inverted witness**
  `a_real_libaom_monochrome_key_frame_decodes_pixel_exact` and its helper
  `ffmpeg_decode_gray_sequence`, in its place.
- `crates/ec-av1/src/decode.rs` and `crates/ec-av1/examples/decode_probe.rs`
  auto-merged; the resolved tree's `crates/ec-av1/` is **byte-identical to the
  lane's** (`git diff 66a39803 -- crates/ec-av1` empty), i.e. `main`'s lineage
  never carried the mono model, so there was no `main`-side decode state to
  preserve. Marker grep clean. All `lanes/*.md` reports kept (union:
  `av1decode`, `av1mono`, `mp4rotation`, `vp9444`).

### Merged-tree gates

- `cargo test -p ec-av1 --lib` to completion → **592 passed, 0 failed,
  60 ignored** (8124 s). Same `592/0/60` as the lane and the pilot — the pin
  swap is count-neutral (one test removed, one added).
- `cargo check -p ec-av1 --all-targets` → **0 warnings**.
- Merged-build spot-check with `decode_probe` (release, merge target dir):
  - (a) gray 60 f key frame → `OK: 60 frames decoded, 320x240`, `gray60 EQUAL True`
  - (b) gray 120 f inter → `OK: 120 frames decoded`, `gray120 EQUAL True`
  - (c) `av1-monochrome.ivf` remuxed to OBU → `OK: 60 frames decoded`,
    `mono.ivf EQUAL True`
  - (d) film window `p-hgb3300` (3840x1608, 10-bit) → `OK: 279 frames decoded`;
    probe sha256 `b17e9036cbcfa90b` == ref sha256 `b17e9036cbcfa90b` (MATCH).

### Cleanup

- Removed worktree `edith_codecs-av1mono`; branch `lane-av1-mono` kept.
- Removed private target dirs `$HOME/.cache/cargo-target-av1mono`,
  `-av1monoverify`, `-av1monomerge` and TMPDIR `$HOME/.cache/tmp-av1monomerge`.
