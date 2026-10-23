# av1safe: ec-av1 forbids unsafe code

## Outcome

`crates/ec-av1` now compiles under `#![forbid(unsafe_code)]`. `par::Shared` is
gone, `par.rs` carries no `allow(unsafe_code)`, and no `unsafe` block, fn or
impl is left in the crate. Clippy with `-D warnings` is clean on the server. The
full ec-av1 gate matches the expected counts. The decode benchmark is a
**regression**: the lane is about 34% slower than base on the default-thread
decode (median 15.60 s vs 11.60 s), well past the 3% threshold. The cause is not
isolated here.

## Approach

The effort ran as a series of lanes, each removing one group of `unsafe` sites:

- **nopipe**: the per-superblock-row pipelined post-filter was deleted; the
  whole-frame filter path is the only one. One pipe-vs-serial witness test was
  deleted with it.
- **nowave**: the opt-in wavefront reconstruction was deleted; inline
  reconstruction writes through plain `&mut`, removing `ReconPlanes` and
  `PredBuild` unsafe. Two t04 tests were deleted.
- **cdef**: whole-frame CDEF writes through `split_at_mut` band slices.
- **bands**: deblock, restoration and film-grain band writes through split
  slices (helpers `split_bands_mut`, `carve_ranges`, `run_bands_mut`,
  `run_bands_with`, `run_parts_mut` in `par.rs`).
- **pool2**: `Batch::submit` no longer transmutes borrowed jobs. `Batch` takes
  `'static` jobs only; borrowed jobs go through `par::scope` on
  `std::thread::scope`.
- **final** (this lane): merged pool2 into the band code. The two `Batch`
  helpers that still took borrowed closures (`run_bands_with`, `run_parts_mut`)
  now use `par::scope`. Deleted `par::Shared` (struct, unsafe impls, `get`),
  the unused `run_bands`, and the `#![allow(unsafe_code)]` in `par.rs`. Changed
  `lib.rs` to `#![forbid(unsafe_code)]`. Refreshed stale `deny`/SAFETY comments
  in lib.rs, `stream/tests/{support,t09}.rs`, and `encode/inter_search.rs`.

Merge of pool2 into the lane had no textual conflicts; the only semantic fix
was moving the two borrowed-job helpers from `Batch` to `scope`, which the
compiler forced.

## Files

- `crates/ec-av1/src/lib.rs`: `#![forbid(unsafe_code)]`, module doc rewritten.
- `crates/ec-av1/src/par.rs`: `Shared` and `run_bands` deleted; `run_bands_with`
  and `run_parts_mut` moved to `scope`; `allow(unsafe_code)` removed.
- `crates/ec-av1/src/encode/inter_search.rs`, `stream/tests/support.rs`,
  `stream/tests/t09.rs`: comment updates only.
- Commits on `lane/av1safe`: `0d86c98b` (final), merge of `lane/av1safe-pool2`
  before it.

## Remaining unsafe

None in `crates/ec-av1`. `grep -rn 'unsafe' crates/ec-av1` returns 8 lines,
all comments or the `#![forbid(unsafe_code)]` attribute itself.
`crates/ec-va` and `crates/ec-hw` contain no `unsafe` code lines either; they
are outside this lane and not built on the servers (see Tests).

## Tests

Gate: `EC_MEMGUARD_MAX=12G EC_AV1_AOM_DIR=$HOME/fixtures/av1-aom EC_REQUIRE_FIXTURES=1
cargo test --release -p ec-av1 --no-fail-fast -- --test-threads=4`, server host c.

| binary | passed | failed | ignored |
|---|---|---|---|
| lib | 815 | 0 | 49 |
| aom_conformance | 1 | 0 | 0 |
| fuzz_regressions | 5 | 0 | 0 |
| real_library | 0 | 0 | 1 |
| worker_panic | 2 | 0 | 0 |
| yuv422_dav1d | 1 | 0 | 0 |
| doctests | 0 | 0 | 0 |

Clippy: `cargo clippy -p ec-av1 --all-targets -- -D warnings` clean.

Targeted before the full gate: `aom_conformance` 1/0, `worker_panic` 2/0,
lib `thread` filter 10/0.

Workspace check: `cargo check --workspace --all-targets` fails in the
`cros-libva` build script on the server (libclang cannot find `stddef.h`).
`ec-va`, `ec-hw` and `ec-bench` depend on it. With those three excluded,
`cargo check --workspace --all-targets --exclude ec-va --exclude ec-hw --exclude ec-bench`
finishes cleanly. The libva failure is the server toolchain, not this change.

## Bench

`decode_bench` on `~/.cache/av1decbench-clip/t1080.obu` (300 frames), default
threads, both trees built and run on server host c, 3 alternating runs each.
Base is `/home/user/.cache/wt/safesimd-av1-core-base` at `c3430e71`.

| tree | run 1 | run 2 | run 3 | median | fps median |
|---|---|---|---|---|---|
| base | 11.601 s | 11.666 s | 10.961 s | 11.601 s | 25.72 |
| av1safe | 15.528 s | 15.597 s | 15.633 s | 15.597 s | 19.23 |

Delta: +34.4% on the median (slower). The spread inside each tree is small
(about 1% for av1safe, about 6% for base), so this is not host noise.
Two earlier lanes measured on a loaded host showed no regression; this
measurement is on a quiet host and does not reproduce that. The likely
suspects are the removal of the pipelined post-filter and wavefront
reconstruction, and the per-stage `thread::scope` spawns in the `par` helpers.
Neither is isolated here. This needs a follow-up before merge.

## Lane history

- Earlier bench figures in the lane reports (pool2 +2.2%, nowave -9.6%, cdef
  -1.5%) were run on loaded hosts and are superseded by the table above.
- The `bands` lane report is not in `lanes/`; its history is in the merge
  commit `0e782901`.
