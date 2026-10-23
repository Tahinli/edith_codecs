# av1safe-wave2: superblock-row wavefront reconstruction (safe Rust)

## Outcome

The superblock-row wavefront is restored in `crates/ec-av1` in safe Rust, with no `unsafe`
and no `#[allow(unsafe_code)]`. Output stays byte-exact. The performance target is **not met**:
on host b (quiet, load about 1.2) threaded t1080 decode is 11.57 s median for wave2 against
8.41 s for base b1540d8b (target was within 5%, about 8.5 s). Wave2 is 12.6% faster than its
parent 7c5553b8 (13.24 s), up from about 7% in the previous attempt.

## Approach

- Parsing stays on the tile thread. Reconstruction writes are queued as owned ops and run by
  workers. Row `r` belongs to worker `r % nw`, and each superblock is one job.
- Each worker owns a full-size private copy of each plane, seeded with its own rows and copied
  back at the end of the tile. No plane is shared, so there is no `unsafe`.
- Halo: after superblock `(r, c)` a worker publishes its bottom pixel row per plane into a shared
  line buffer under `Mutex` + `Condvar`. Superblock `c` of row `r` waits until row `r-1` has
  finished column `c+1` (above-right), then copies that segment into its local halo row.
- Changed in this attempt: the inverse transform (`TxParams::run`) is deferred to the worker
  (`ReconOp` carries `Residual::Tx(tx, grid)`), not run on the parse thread. This is the same
  split base uses. On its own it measured as neutral, because the parse thread is still the
  bottleneck.
- The wave is used only for inter-frame tiles, and it is off for intrabc frames, when
  `recon_threads() < 2`, and for tiles with fewer than 2 superblock rows. Key-frame tiles stay
  inline, as base does.
- Inter-intra: `wave::finish_tile` joins the workers at the inter-intra syntax sites, then the
  rest of that tile is inline. Base instead barriers and resumes. Measured on t1080: this path
  never fires (0 mid-tile finishes out of 1236 wave tiles), so coverage does not explain the gap
  on this clip. It may matter for other streams.

## Measured bottleneck (this attempt)

Quiet host b, EC_AV1_TIMELINE and per-worker counters:

- Parse thread (tile loop) is about 10 s wall of a 11.7 to 12.5 s decode. Workers are idle
  about 70% of the time waiting for jobs (`recv` wait 21.8 s summed over three workers), so
  the wavefront is not limited by reconstruction or by halo waits.
- Per-frame timeline: frame N+1 waits for frame N's whole filtered picture (`start_to_refs`).
  The frame chain is therefore tile time plus filter time. Base's tile phase averages about
  25 ms per frame (7.6 s total) against about 37 ms for wave2 (11.6 s total). Base's gain
  comes from parse-side work it moves into workers, plus row-level reference progress.
- Parse-side work that base defers and wave2 does not:
  - Inter prediction (`mc::with_scratch` time on the parse thread): about 1.5 to 1.9 s.
    Base's `ReconOp::Pred` builds predictions in the worker. Doing that here needs owned
    reference pictures (`RefPix` borrows its pictures today) and changes at about eight
    `build` closures and their call sites, so it is not done in this lane.
  - Copies of residuals and predictions into owned ops: about 1.1 to 1.2 s. These are memcpy
    bound (about 18 MB per frame). Removing them needs the coefficient arena moved wholesale
    into the job, as base does, rather than a copy per block.
- Recycling buffers (a free list returned by the workers) was tried and gave no measurable
  change, so it was removed.
- Full-size private planes (item (a) of the coordinator's list): not changed. Seeding and
  copy-back cost about one frame copy per tile, which is small next to the copies above. An
  in-place band design would need `PlaneBuf` to address rows relative to a band origin, across
  about 239 `.data` sites. Judged too large for this pass.
- Sync (item (c)): the per-superblock `Mutex` + `Condvar` is about 510 handoffs per frame and
  is not the bottleneck.

## Bench

`decode_bench` on `~/.cache/av1decbench-clip/t1080.obu`, host b, default threads unless noted.
Three interleaved rounds, all on load about 1.0 to 1.4 (per-run seconds):

| Tree | R1 | R2 | R3 | Median |
|------|----|----|----|--------|
| base b1540d8b | 8.41 | 8.60 | 8.27 | 8.41 |
| base b1540d8b, EC_AV1_RECON_THREADS=1 | 13.42 | 13.23 | 12.96 | 13.23 |
| parent 7c5553b8 | 13.44 | 13.23 | 13.24 | 13.24 |
| wave2 (this lane) | 11.57 | 11.74 | 10.85 | 11.57 |
| wave2, EC_AV1_RECON_THREADS=1 | 13.40 | 13.02 | 13.19 | 13.19 |

The coordinator's quiet-host numbers (base 8.11, wave2 12.37) were measured separately; the
table above is this attempt's own run.

## Files

- `crates/ec-av1/src/decode/wave.rs`: wavefront scheduler, workers, halo, `Residual`.
- `crates/ec-av1/src/decode/mod.rs`: deferral branches in `push_intra_tx`, `push_mc_rect_tx`,
  `exec_intra`, `exec_mc`.
- `crates/ec-av1/src/decode/frame_tile.rs`: install, per-superblock `begin_sb`, `finish_tile`.
- `crates/ec-av1/src/decode/inter_block8.rs`, `inter_block.rs`: inter-intra `finish_tile`.
- `crates/ec-av1/src/par.rs`: `recon_threads()` and test-only `set_recon_threads()`.
- `crates/ec-av1/src/autothreads.rs`: docs only (formula unchanged).
- `crates/ec-av1/src/stream/tests/t04.rs`: restored recon-threads test.
- `crates/ec-av1/tests/worker_panic.rs`: restored `("recon", 3)` site.
- `Cargo.lock`: unchanged.

## Remaining unsafe

None in `crates/ec-av1/src`. The only matches for `unsafe` are `#![forbid(unsafe_code)]` in
`lib.rs` and comments in `par.rs`, `encode/inter_search.rs`, and `stream/tests/support.rs`.

## Tests

Targeted (this attempt): `aom_conformance` 1 passed, `worker_panic` 2 passed,
`a_real_stream_reconstructs_identically_with_one_and_four_recon_threads` passed.

Full gate (`cargo test --release -p ec-av1 --no-fail-fast -- --test-threads=4`, server):
- lib: 816 passed, 0 failed, 49 ignored
- all integration binaries: 0 failed (aom_conformance 1 passed, worker_panic 2 passed)
- `cargo clippy -p ec-av1 --all-targets -- -D warnings`: clean

## Attempt 3: prediction deferral (not implemented, with reasons)

Scoped and measured, not built. The code is unchanged from `82c36b12`.

- Refs: `Picture` is already held as `Arc<Picture>` in the threaded stream (`SlotValue.pixels`),
  so sharing refs with workers is cheap. What is missing is `RefPix`: it borrows `&'p Picture`
  from a per-frame `OnceLock` and needs an `Arc`-yielding wait closure alongside the borrowed one.
- Predictions: about eight `build` closures (`inter_block8.rs`, `inter_block.rs`, `sub8_inter.rs`,
  `leaf_sub8.rs`) capture `&RefPix` and run inline. Their outputs feed `Pred::Inline` at push
  sites, and some sites read prediction values at parse time (the sub8 chroma combination,
  inter-intra, OBMC, and masks). Each deferrable site needs a worker-side slot ID, and the
  parse-time readers must stay inline.
- Coefficients: `push_*_tx` receive `grid: &[i32]` from `Grid` values built in `block.rs`.
  Moving them into a per-SB arena needs offsets instead of slices there.

Ceiling from the existing instrumentation (`with_scratch` timer on the parse thread, 300
frames of t1080): MC about 1.5 to 1.9 s and residual copies about 1.2 s, against a wave2 median
of 11.57 s. Removing all of it would give about 9 to 10 s, which is still above the 8.5 s target,
because base also overlaps frame tails through row-level reference progress, which this tree
does not have.

## Attempt 4: prediction deferral built, zero-residual marker, coefficient arena (target met)

Result on host b, t1080, default threads, 3 interleaved rounds (median seconds):

| Tree | R1 | R2 | R3 | Median |
|------|----|----|----|--------|
| base b1540d8b | 8.62 | 7.99 | 8.45 | 8.45 |
| base, EC_AV1_RECON_THREADS=1 | 13.90 | 13.89 | 13.57 | 13.89 |
| final (this lane) | 8.80 | 8.20 | 8.84 | 8.80 |
| final, EC_AV1_THREADS=1 | 13.37 | 13.20 | 13.20 | 13.20 |

An earlier 6-round interleave gave base 7.96 and final 8.12 (+2%); this table is +4.1%. Both are
within 5%.

What changed (by measured benefit; wave2 before: 11.57 s):

1. Block predictions are built on the recon workers (11.57 -> about 9.1 s). The decisive effect
   was not the MC cost on the parse thread but the reference wait: a parse-time MC blocked on the
   previous frame's filtered pixels, so a frame could not parse over its producer's filter tail.
   Now the parse thread never needs a pixel, as in base (the two-stage design). Mechanism:
   - `RefPix` carries an optional `SharedRefs` (dims plus an `Arc` wait returning `Arc<Picture>`),
     built in `stream/mod.rs` from the frame's `ref_metas`. The wavefront workers wait for the
     pictures themselves and build a `RefPix::lazy` over them.
   - The twelve `build` closures (inter_block8, inter_block, sub8_inter) take `refpix` as a
     parameter instead of capturing it. They were already `Send + 'static`, so
     `wave::ReconOp::Pred(Box<dyn FnOnce..>)` needs no lifetime laundering. A deferred block
     leaves its prediction in a per-worker slot per plane and `Pred::Cur` windows into it.
   - Deferral applies only when a wavefront is running and the `RefPix` is shared;
     inter-intra still finishes the wavefront and continues inline.
   - A worker's reference-wait failure is parked on its `RefPix` and carried to the tile's
     `RefPix` at `finish_tile`.
2. Zero residuals (skipped blocks, all-zero grids) travel as a marker instead of a 64 KB copy
   (about 9.1 -> 8.6 s).
3. Coefficients go into a per-superblock arena that is handed to the worker whole, with
   offsets in the ops (8.6 -> about 8.1 to 8.8 s, noise-level on a single run, parse phase on the
   critical path 26.5 -> lower).

Not done, not needed: row-level reference progress and the pipelined filter. Base's timeline
also shows `pipelined_filters=0/310` on this clip, and base waits for the whole reference
picture too.
Remaining difference: the whole-frame filter phase is about 1.7 ms/frame slower than base
(6.6 vs 4.9 ms median, from the earlier safe band-filter lanes), which is the main residual
of the 2 to 4%.

Full gate (server a): lib 816 passed, 0 failed, 49 ignored; all integration binaries 0 failed. Targeted: `aom_conformance` 1 passed, `worker_panic` 2 passed, lib filter `thread` 11 passed,
t04 recon-thread test passed, clippy `-D warnings` clean.
