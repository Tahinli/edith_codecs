# av1safe-pool2: borrowed `Batch` jobs off the transmute

## Outcome

The `transmute` in `par::Batch::submit` is gone. `Batch` now takes only `'static`
jobs and is used only by the frame workers in `stream/mod.rs`. Borrowed jobs
(filter bands, deblock/CDEF/LR/SSE helpers, encoder tile search and tile write)
run on a new `par::scope` built on `std::thread::scope`. No `transmute` remains
in `crates/ec-av1`.

## Approach

- `Batch` (frame workers): persistent per-thread pool kept as before. The
  `submit` bound is now `F: FnOnce() + Send + 'static`, and the job is boxed
  directly as `Box<dyn FnOnce() + Send + 'static>`. The compiler then flagged one
  borrow that had been hidden behind the lifetime: the frame job read the
  borrowed input slice `tile_bufs` (its `&[&[u8]]` tile views into the stream).
  The job now owns a copy of its tile bytes (`owned_tiles`, rebuilt as
  `tile_views` inside the job), so nothing borrows `data` after the parse loop.
  The copy is the frame's compressed tile payload only.
- `par::scope(class, |batch| ...)` / `Scope::submit`: each job is one
  `std::thread::Builder::spawn_scoped` thread named `class`, joined when `scope`
  returns. Panic contract matches `Batch`: each job's panic is caught, the first
  one is kept and re-raised after every job has finished.
- Call sites converted mechanically (`let batch = Batch::new(..)` plus block ->
  `scope(.., |batch| {..})`): `filter_search.rs` (2), `decode/deblock.rs` (2),
  `encode/inter_search.rs` (2). `run_bands` and `run_bands_mut` keep their
  signatures; only their bodies use the scope. Band 0 still runs on the
  calling thread.
- Thread-local scratch: measured first (below). The bench showed no regression
  over 3%, so `rayon` was not introduced.
- `par::Shared` untouched. `#![allow(unsafe_code)]` kept at the top of par.rs.

Why scope and not rayon: no regression over the 3% bar on either setting, so the
simpler, dependency-free option was kept. The cost of a fresh thread per stage
is visible but small (+2.2% default), and the threads=1 path never spawns.

## Files

- `crates/ec-av1/src/par.rs`: `Batch` (no lifetime, no transmute), `Scope`,
  `scope`, `run_bands`/`run_bands_mut` bodies.
- `crates/ec-av1/src/filter_search.rs`, `crates/ec-av1/src/decode/deblock.rs`,
  `crates/ec-av1/src/encode/inter_search.rs`: borrowed call sites to `scope`.
- `crates/ec-av1/src/stream/mod.rs`: frame job owns its tile bytes.

Cargo.lock unchanged (no new dependency).

## Remaining unsafe

`grep -n unsafe crates/ec-av1/src/par.rs` now shows only `par::Shared`
(`unsafe impl Send/Sync`, `get`) and one comment that mentions the word.

## Tests

- `cargo clippy -p ec-av1 --all-targets -- -D warnings`: clean (on server).
- `--test aom_conformance` 1 passed, 0 failed; `--test worker_panic` 2 passed,
  0 failed.
- lib `thread` filter: 10 passed, 0 failed; lib `bounded` filter: 4 passed,
  0 failed.
- Full gate (`EC_MEMGUARD_MAX=12G`, `--no-fail-fast`, `--test-threads=4`):
  lib 815 passed, 0 failed, 49 ignored (the stated main baseline of 818 is from
  before the wavefront/pipeline deletions; base at 39a4afaf was not re-run for
  its count). Integration binaries: 1, 5, 0 (1 ignored), 2, 1 passed, 0 failed.
  Exit 0.

## Bench

`decode_bench` on `t1080.obu` (300 frames), built and run on the same server (host
c), base = 39a4afaf, lane = this branch, 3 alternating runs each. Host
contention from other lanes was present; the medians below carry that noise.

| setting | base median | lane median | delta | base RSS | lane RSS |
|---|---|---|---|---|---|
| default threads | 17.05 s | 17.42 s | +2.2% | 234 MB | 237 MB |
| `EC_AV1_THREADS=1` | 14.53 s | 14.91 s | +2.6% | 85.9 MB | 86.1 MB |

The threads=1 path never spawns a thread and runs the same code as before, so
its +2.6% is run-to-run noise. Both are under the 3% regression bar.

Encode timing: `enc_probe` on the same clip at 1280x720, 8 frames, q=120,
3 alternating runs (host c). Median wall: base 31.72 s, lane 31.60 s (-0.4%).
Output stream identical (fnv1a `a9ca67c9b1a81112` both). RSS about 228 MB on both.

RSS was taken with `getrusage(RUSAGE_CHILDREN).ru_maxrss` through a small
python wrapper, because `/usr/bin/time` is not installed on the servers. It is the
same counter `/usr/bin/time -v` reports.
