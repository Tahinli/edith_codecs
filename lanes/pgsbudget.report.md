# pgsbudget: bound the ec-pgs pending queue, speed check

Base: main c0347b7f. Branch lane/pgsbudget.

## Task 1: how edith uses ec-pgs (read only, not edited)
- The only call site is `CueImage::rgba` in `crates/engine/src/subtitle.rs` (edith tree).
- It builds a fresh `PgsDecoder` per subtitle block, pushes that one block (one display set, cut at
  the first END that shows), takes one frame, and drops the decoder.
- Edith never pushes many segments before draining. Its exposure to the queue is one frame.

## Task 2: bounded queue (crate change)
- Finished display sets are now stored lazily. A finished set is a `Pending` of its canvas size and
  its placed objects, with palette and objects shared through `Arc`. The canvas is drawn in
  `take_frame`. Before, every finished set held a full 1920x1080 canvas (about 8.3 MB).
- The queue is capped at `MAX_PENDING_SETS` = 8, and `pending_frames()` reports its length.
- API change: `PgsDecoder::push(&[u8]) -> Result<usize>` (was `Result<()>`). It returns how many
  bytes it consumed. It stops early only when the queue is full at a display set's END, and the
  caller then takes frames and pushes `bytes[n..]`. An `Err` still means every byte was consumed.
  A segment error is returned by the call that consumes the rest of its input, as before.
- `Decoder::send_packet` keeps the unconsumed remainder of a packet internally, and
  `receive_frame` resumes it. `send_packet` returns `NeedMore` if called while that remainder is
  pending, so the caller receives first and the packet is never resent.
- Tests added in `src/lib.rs`:
  - `a_long_stream_in_one_push_keeps_the_queue_bounded`: 3000 display sets in one push. The first call
    stops early. Draining and re-pushing finishes all 3000 frames, and the queue never exceeds 8.
  - `a_full_queue_holds_the_last_end_until_a_frame_is_taken`: the held-back END completes on the next take.
- The existing tests (including the `tests/fuzz_regressions.rs` seeds) pass unchanged.
- Not measured: process RSS for a single-push 2 h file. The queue bound is what the test asserts.
- Residual: the object and palette maps are not capped. They are reset at each epoch start, and
  their size follows the stream's distinct object ids (at most 65536) and their data. This is not
  the pending-output queue, and it is not capped here.

## Task 3: speed
- Local timing, 2 h synthetic .sup (the benchp1 generator), best of 5, 1 thread, start load 1.47:
  16.29 s, 0.99 MB/s, 442x. Before: 16.5 s, 437x. No real change.
- Cause: canvas allocation and zeroing per display set. 14400 sets of 8.3 MB is about 119 GB of
  writes, at roughly 7 GB/s. Two probe builds on host a showed it:
  - Skipping the whole rasterize step: 3.6 ms.
  - Allocating and returning each canvas but skipping the draws: 16.3 s.
  - So the draws (RLE, pixel writes) are negligible, and the allocation and zeroing are the cost.
- perf was not usable: the local kernel refuses perf events (perf_event_paranoid=2) and host a has no perf.
- Not fixed, by design: the public output is a full-canvas RGBA frame, so each set writes 8.3 MB no
  matter how the decoder is written. Going faster needs an output that covers only the set's
  bounding rectangle (a new method, with an edith change to use it). That is an API decision, so
  it was not made here.
- Bench row added under `## lane pgsbudget` in lanes/bench.md.

## Task 4: gates (host a, server)
- `cargo test --release -p ec-pgs`: 7 unit tests passed, 2 regression tests (`tests/fuzz_regressions.rs`)
  passed, 0 failed.
- `cargo clippy -p ec-pgs --all-targets -- -D warnings`: rc=0.
- `lib.rs` is 771 lines, under 5000.
- Fuzz targets were not run for an hour. The regression seeds are the test file above.
- Edith compile check: `cargo check --lib` in the edith engine crate failed at the `rav1e` build script
  on host a, an unrelated dependency. The call site was checked by reading: `decoder.push(&sup).ok()?;`
  compiles with the new `Result<usize>` (the count is discarded).
- Edith change needed: none for correctness. A block is one display set, so the count is always full.
  Optional: `rgba()` could assert the count is the block length, but that is not required.

## Task 5: commit
- Commit on lane/pgsbudget: `lane/pgsbudget: ec-pgs lazy display sets with a bounded queue`.

## Left
- Speed: an output that covers the set's bounding rectangle (see task 3).
- Object and palette maps are uncapped (see task 2 residual).
- No DONE.md criteria were given to this lane.
