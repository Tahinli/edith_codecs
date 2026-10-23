# lane/hwlost report

Branch lane/hwlost, one commit (6492dbdc) on fe27399c. Files: crates/ec-hw/src/{frame.rs,error.rs,dec/mod.rs}, crates/ec-va/src/{surface.rs,sys.rs}.

## Root cause

Two separate things.

1. **The VCN hang.** A damaged H.264 stream makes the hardware decoder hang the VCN ring. The ring job times out after about 2 s and the kernel resets the ring (`ring vcn_unified_0 timeout ... reset succeeded`).
   - The input is `assets/test_25fps.export.mp4` (1280x720 H.264 Main, CABAC, multi-slice), mutated by media_fuzz. It is a new gitignored asset (mtime 04:08), and the first timeout was at 04:10. Before that there were 0 VCN timeouts in 7 days of journal. media_fuzz picks up every file in `assets/`, so this file entered its corpus at 04:08.
   - The guilty cases are `test_25fps.export-13.mp4` (the worst, it hangs in roughly 1 of 3 runs) and `-14`/`-15`. Their damage is 5 to 20 flipped bytes inside the slice data of the first AUs. NAL headers, SPS/PPS and slice headers are intact. All five flipped bytes in -13 are deep in the slice payloads of AU 0, 1 and 3, so entropy-coded data is corrupt and nothing in the headers is odd.
   - The hang is stochastic. The same bytes hang the ring in some runs and decode fine in others. Pre-validating the bitstream is therefore not possible short of full CABAC decoding, so I did not add ec-h264-syntax/ec-h265-syntax pre-checks. The headers were valid.
   - Minimal repro: `ec-hw` `Decoder` fed the first 5 AUs of the damaged file (27562, 17131, 19250, 21304 and 20974 bytes). It hangs in AU 0 (the IDR) in about 4 of 8 runs.

2. **The SIGABRT.** The abort is not in our Rust code. gdb on the core dump shows `abort()` called from `amdgpu_ctx_set_sw_reset_status` -> `amdgpu_cs_submit_ib` inside Mesa 26.2.4's `radeonsi_drv_video.so`, on Mesa's own `util_queue` thread, so `catch_unwind` cannot stop it.
   - Mesa aborts on the next command-stream submit to a context the kernel has reset ("The CS has cancelled because the context is lost. This context is innocent.").
   - So after one hung job, anything that submits on a reset context kills the process. That includes the next fuzz case's decoder, since the previous case's job is still hanging when the next case starts.
   - The `frame.rs:167` panic is the same event seen from the readback side: `vaGetImage` succeeds on a lost context but the mapping does not cover the frame, and `y_plane[src..src + w]` panics.

## Regression?

No code regression. Evidence:
- `fe27399c` (main) reproduces the abort with the plugin built from that tree.
- The trigger is the new asset (04:08) plus the stochastic hardware behaviour. I did not run the old commit 2fcdcdb5. The engine at main HEAD would not build against it, and the new-asset timing already explains the 04:10 start.
- Kernel and Mesa did not change at that time. Boot was 19:43 yesterday on 7.2.11-200, and the last dnf update was 2026-10-19 16:53.

## Fixes (committed)

- **ec-hw/frame.rs.**
  - Both readback paths (`to_i420`, `to_i420_16`) now go through `fill_i420` and `fill_i420_16`, which first run `check_plane` on luma and chroma. `check_plane` checks the plane length, pitch, row bytes and checked arithmetic, covering 8-bit and 10-bit. A short or odd mapping returns `Error::Config`. It is no longer a slice panic.
  - Tests: `a_plane_that_covers_the_frame_passes`, `a_short_or_odd_plane_is_an_error_not_a_panic` and `short_mappings_are_refused_on_every_path`, with fabricated short mappings on all 4 paths at 8 and 10 bit.
- **ec-hw/dec/mod.rs, Session::submit.** This is the watchdog/poison.
  - After each picture is submitted it calls `Surface::sync_timeout` (new, uses `vaSyncSurface2`, 8 s cap, falls back to `vaSyncSurface` when the driver lacks it).
  - A sync error, a sync that returns after more than 1.5 s (a ring reset), or a begin/render/end status of INVALID_CONTEXT, OPERATION_FAILED, HW_BUSY, TIMEDOUT or DECODING_ERROR sets `poisoned`. After that every `submit` returns `Err(Error::poisoned(..))` without touching VA, so the process never submits to the reset context.
  - Cost: per-picture sync instead of a pipelined submit. Bench on the 720p asset went from 1680 fps to 1416 fps (47x realtime), a 17% hit.
  - Frame readback also uses `sync_timeout` now.
- **ec-va.** `Surface::create` no longer has an `expect`. `sys.rs` declares `vaSyncSurface2` and the status constants.
- **Audit result.** Nothing else needed changing.
  - All VA calls already go through `check()`.
  - `vaDestroy*` and `vaUnmapBuffer` in every Drop ignore their status and cannot panic.
  - There is no `panic = "abort"` profile, only `[profile.release] opt-level = 3`.
  - The remaining unwrap/expect/index sites in ec-hw/ec-va are in tests or are bounds-checked (PRIME descriptor counts are checked before slicing).

## Edith diffs (not applied, for the edith session)

The plugin now returns an error and then keeps erroring on every `vh_next_frame` for that session, and the ring recovers. In `crates/engine/src/decode.rs` `run_hw`, an `Err` after the first picture only sets `failure` and ends the span (`return true`). Edith should hand the rest of the span to software instead. Sketch:

```diff
-fn run_hw(hw: &mut HwSession, pass: Pass) -> bool {
+/// `Some(index)` when hardware failed mid-span and software should take over from `index`.
+fn run_hw(hw: &mut HwSession, pass: Pass) -> HwEnd {
 ...
             Err(e) => {
-                if index == start_frame {
-                    return false;
-                }
-                failure.set(format!("hardware decode error at frame {index}: {e}"));
-                return true;
+                if index == start_frame {
+                    return HwEnd::Refused;
+                }
+                eprintln!("hardware decode error at frame {index}: {e}; continuing in software");
+                return HwEnd::FellBackAt(index);
             }
```

The caller then seeks the software seat to `index` and continues the same span. Without it a mid-stream hang ends the clip's decode with a failure on the strip, and the engine is otherwise safe.

Separately, media_fuzz's `drain(rx, 3)` drops the session while the decoder thread is still running. That is fine now (every submit is synced before the next one), but it means the thread can still be inside a hung job when the next case starts, which is how an *innocent* context aborts. The poison fix makes the guilty session stop submitting. The kernel reset, though, lasts about 2 s, and any other process's VA context (browser, mpv) alive then still aborts if it submits during that window. That is a Mesa behaviour outside this repo.

## Gates

- `cargo clippy -p ec-hw -p ec-va --all-targets -- -D warnings`: clean.
- `cargo test -p ec-hw -p ec-va --release -- --include-ignored`: all pass (ec-hw 22 unit + 41 gpu + 3 + 2 + 1; ec-va 8 + 14 + doctests).
- media_fuzz in the edith copy (assets copied in, so the corpus is 116 files), release build:
  - Default 16 iterations: 1904 cases x 3 runs, all `ok`.
  - 48 iterations: 5712 cases, `test result: ok`, no panic, no abort.
  - These full runs happened to see no ring timeout (stochastic).
  - Targeted: the 32 cases up to the guilty one, 4 runs, plus export-13 alone, 3 runs, all with the patched plugin. Kernel log during them: `ring vcn_unified_0 timeout, signaled seq=... emitted seq=...` at 04:56:12, :18, :24, :27, :33, :41 and :47, i.e. the hang did reproduce 7 times, and every run still finished `ok` with no SIGABRT.
  - Before the fix, the same sequence aborted in 3 of 3 runs (`signal: 6` plus "context is lost").
  - Teardown example, 8 runs: poison message (`a picture took 2.4s (limit 1.5s)`), no abort.

## Cleanup

Removed ~/.cache/wt/edith-hwlost, ~/.cache/wt/hwlost-edith, ~/.cache/wt/hwlost-base, the repro tmp files and the extra target dir.
