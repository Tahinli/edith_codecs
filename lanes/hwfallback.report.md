# lane hwfallback report

Branches: edith_codecs `lane/hwglobal` e1f1d968 (worktree ~/.cache/wt/hwglobal), edith `lane-hwfallback` 50b04467 (worktree ~/.cache/wt/edith-hwfallback). No trailers on either commit. hwfb-pair removed. Nothing pushed.

## Task 1: process-wide poison (ec-hw)

- New `crates/ec-hw/src/reset.rs`: a global epoch counter plus the time of the last reset. Public API: `report_gpu_reset()`, `gpu_reset_epoch()`, `gpu_recently_reset()`, `GRACE` = 3 s. New `Error::HwReset` with `Error::hw_reset()` / `is_hw_reset()`.
- Every decode `Session` and `Encoder` remembers the epoch it was created in. Once the epoch moves, `submit` / `encode` return `Error::HwReset`, so a context the kernel reset along with the guilty one is never submitted to.
- `Session::new` and `Encoder::new` refuse for 3 s after an event (HwReset). After that, new sessions work normally.
- `Frame::to_i420*`, `export_prime` refuse frames decoded before a reset (their surfaces belong to a lost context).
- Every picture wait (`sync_watched`) reports a reset when the wait errors or takes more than 1.5 s. Both submit-time and readback waits do this. Statuses DECODING_ERROR / OPERATION_FAILED on submit stay a local poison only (a bad stream must not condemn the process).
- DRM `AMDGPU_CTX_OP_QUERY_STATE2` was considered and not used: the amdgpu context id belongs to Mesa's winsys, ec-va has only the render-node fd, so there is nothing to query. `vaQuerySurfaceStatus` does not report resets. The epoch flag is the mechanism.
- Residual race: a different, innocent session that submits in the window between the kernel finishing the reset and the guilty session's wait returning is not covered. The window is the tail of the 2 s reset and the guilty session's wake-up. No cheap fix exists without a watcher thread.
- Depth-1 pipelining: `submit` renders picture N, then waits for picture N-1. A hung picture is waited for right after the next submit (ring cannot be reset yet, the job timeout is seconds), and a pending picture older than 250 ms is settled before anything is submitted. Frame readback also watches its wait. Residual: a picture that is never read back and is followed by an idle gap before the next submit is only checked at that next submit.
- Throughput, 720p H.264 Main, 350 pictures, `bench dec h264`, best of 5 (two sets for the pipelined version):
  - before (hwlost per-picture sync): 1255 fps (41.8x realtime), runs 1207-1255
  - after (depth-1): 1519 and 1429 fps (50.6x / 47.6x realtime), about +15 to +20%
- Tests: 5 unit tests of the epoch clock (`reset.rs`), and `crates/ec-hw/tests/reset.rs` on the real GPU: live sessions die with HwReset, a stale frame is refused, a session created in the grace period is refused, and a fresh session decodes after the grace period.

## Task 2: mid-span software fallback (edith)

- `crates/engine/src/decode.rs`: `run_hw` returns `HwEnd::{Done, Refused, FellBackAt(index)}`. `run_span` keeps the old Refused behaviour (reopen after an unfollowed reseek, else software from `start`) and on `FellBackAt(index)` drops the session and runs the codec's software seat (ec-h264 / ec-h265 / ec-av1 / ec-vp9 / ec-vp8) with `start = index`. No failure is set; one stderr line says it continued in software.
- All playback, scrub, thumbnail, strip and still decodes go through `run_span`, so they are all covered (the only other users of the plugin are decode.rs and export.rs, checked by grep).
- After a GPU reset, `engine-hw` `OpenSession::open_at` returns null for the VA-API codecs while `ec_hw::gpu_recently_reset()`, so `open_hw` gives `None` and the decode takes the software seat (playback, probe, export alike). A kept session whose seek succeeds but whose next picture errors with HwReset takes the Refused path, whose reopen is refused in the grace period, so it also goes to software.
- Test hook: `engine::hw::inject_failure_after(Some(n))` (doc-hidden) or `EC_HW_FAIL_AT=n` makes a session report a hardware error after n pictures (counted from open or seek). Cost when unused: one atomic load per open.
- `crates/engine/tests/hw_fallback.rs`: for H.264 (mp4 full and mid-span start 10, fail at 17/5/49, mkv) and HEVC, the fallback run has the same frame count as pure software and frames from the failure on are byte-equal to a pure software decode. Skips without a VA-API decoder.
- playback.rs: no change needed.

### export.rs hunks (one function, `decode_stream`)

```diff
@@ -8736,9 +8736,8 @@ impl ClipDecoder {
 /// will have it, software otherwise) and send every picture until the encoder
 /// stops taking them.
 ///
-/// Hardware that fails before the first picture falls back to the software seat
-/// (see the loop's `decoded == 0` arm); one that fails after it fails the
-/// export, which then deletes the half-written file.
+/// Hardware that fails, before the first picture or after some, hands the rest
+/// of the span to the software seat at the first picture not yet sent.
 ///
 /// Every way this returns says which way it was, down the channel: a picture, an
 /// error, or the `Ok(None)` that *is* end of stream. Closing the channel is not
@@ -8780,11 +8779,9 @@ fn decode_stream(
             },
         },
     };
-    // Pictures handed down the channel. Hardware that dies *before* the first
-    // one is the fallback playback already makes, and here nothing has been
-    // encoded yet, so the software seat can take the whole span with no frame
-    // repeated. Past one picture a restart would re-encode what the muxer
-    // already holds, which is why the export still fails there.
+    // Pictures handed down the channel. Hardware that dies hands the rest of the
+    // span to the software seat at `start_frame + decoded`: the first picture
+    // not yet sent, so nothing already encoded is repeated.
     let mut decoded = 0u32;
     loop {
         // Asked to stop: the span is over, and a software decoder that has not
@@ -8818,11 +8815,16 @@ fn decode_stream(
                 return None;
             }
             Err(e) => {
-                if decoded == 0 && matches!(decoder, Pictures::Hw(_)) {
-                    if std::env::var_os("EDITH_DEBUG").is_some() { eprintln!(
-                        "export decode: hardware failed before any frame ({e}); falling back to software"
+                if matches!(decoder, Pictures::Hw(_)) {
+                    // Before the first picture, or after some: either way the
+                    // software seat takes the span from the first picture not yet
+                    // sent (`decoded` is 0 in the first case), so nothing is
+                    // encoded twice and nothing is skipped.
+                    if std::env::var_os("EDITH_DEBUG").is_some() || decoded > 0 { eprintln!(
+                        "export decode: hardware failed at frame {} ({e}); falling back to software",
+                        start_frame + decoded
                     ); }
-                    match Pictures::software(path, start_frame, wide) {
+                    match Pictures::software(path, start_frame + decoded, wide) {
                         Ok(sw) => {
                             decoder = sw;
                             continue;
```

## Kernel log lines (targeted repro)

The hwlost source file was moved out of assets, so I regenerated an equivalent: libx264 Main, 4 slices, 720p from assets/test_25fps.mp4, then random byte damage in the slice data of the first 4 AUs, 12 variants x 3 rounds through `DecodeSession::open_range` (private copies in ~/.cache/tmp-hwfallback, nothing written to any assets/).

```
Oct 20 05:20:05 amdgpu 0000:03:00.0: ring vcn_unified_0 timeout, signaled seq=4450442, emitted seq=4450444
Oct 20 05:20:05 amdgpu 0000:03:00.0: Ring vcn_unified_0 reset succeeded
Oct 20 05:20:19 amdgpu 0000:03:00.0: ring vcn_unified_0 timeout, signaled seq=4450446, emitted seq=4450448
Oct 20 05:20:19 amdgpu 0000:03:00.0: Ring vcn_unified_0 reset succeeded
```

Both runs finished with no abort; the first file after the hang took 2.5 s (the sync that detected it), later files went to software during the grace period. Damaged AU 0 is also refused by the software decoder, so those files deliver 0 frames in both seats.

## Gates

- ec-hw/ec-va: `cargo clippy -p ec-hw -p ec-va --all-targets -- -D warnings` clean; `cargo test -p ec-hw -p ec-va --release -- --include-ignored` all pass (ec-hw 27 unit + 41 gpu + 3 + 2 + 1 reset + 1 + 1; ec-va 8 + 14 + doctests).
- edith local (copy with ../../../edith_codecs resolving to hwglobal): clippy -D warnings on engine, edith_mcp, edith, release all-targets clean. `hw_midgop_seek` (h264 and hevc mid-gop) pass, `hw_decode --include-ignored` 13 pass, `hw_fallback` pass. `media_fuzz` 3 runs ok (15-18 s each).
- server host c (no GPU), clippy then `cargo test -p engine -p edith_mcp -p edith --release --no-fail-fast`: clippy clean, everything passes except the 2 VA-API-only tests `h264_mid_gop_seeks_match_a_sequential_decode` and `hevc_mid_gop_seeks_match_a_sequential_decode` (the allowed ones). Log: ~/.cache/tmp-hwfallback/remote.log.
- Local engine tests needed assets, which the worktree lacks; I symlinked the main checkout's files into the worktree's gitignored assets/ for the run and removed the links afterwards.
