# lane-av1intertxwedge — the `an_inter_clip_...` wedge on wave14

Target: `encoder::tests::an_inter_clip_codes_both_inter_set_tx_types_both_decoders_read_exactly`
(`crates/ec-av1/src/encoder.rs:5415`).
Tree: `lane/av1intertxwedge` off `origin/main` = `1bac42c3` (merge of `lane/av1floorshift`).
Local repro host: 12-core workstation, Fedora, `eu-stack` + `gdb` present.

## Verdict

**Not reproduced alone — and not a hang.** The test is a 50-second encode that
finishes deterministically. The VPS wedge is a **`speed::KNOBS` lock convoy** in
the crate's test binary, not a defect in this test: this test is one of the
longest *holders* of that exclusive lock, so on a loaded host libtest's harness
thread parks in `futex_do_wait` while a sibling does real work, and the
*report* names whichever test is printed last. The wedge is the convoy; the
50 s holder is a contributor to convoy length, not the cause.

No code fix is made. The one change that would shorten the convoy (this test
sets the process-global lever `set_inter_tx_search` and is the only such test
NOT carrying `#[ignore = "sets process-global search levers: run it alone"]`,
unlike its two siblings at `encoder.rs:5513` and `encoder.rs:5637`) is
deliberately left alone: adding `#[ignore]` is explicitly out of scope for this
lane, and the ignore would hide a passing gate rather than fix a wait. Logged
as an open decision for Main below.

## 1. What the test encodes, and what it waits on

It builds a 320x192 `Av1Encoder` (gop 4, `base_q_idx` 90, one tile) and encodes
a 4-frame clip of hard 16-px checkerboard steps that MOVE (`x + t*5`, `y + t*3`),
so the motion-compensated residual along a moving edge is where `IDTX` beats
`DCT_DCT`. It first flips a process-global lever:

```rust
let _knobs = crate::speed::knob_write();               // encoder.rs:5419
let _gate_lock = crate::stream::tests::lock_gate_counters();  // encoder.rs:5420
crate::encode::set_inter_tx_search(Some(true));
```

Two exclusive process-global locks are taken and held for the whole body:

- `speed::KNOBS: RwLock<()>` via `knob_write()` (`speed.rs:1058`) — the same
  exclusive lock every preset setter takes. The lever is OFF at every preset
  (`speed::TX_TYPE_SEARCH_INTER`, `speed.rs:886`), so the test must hold it
  exclusive to turn the inter tx-type search on without a racing preset.
- `stream::GATE_COUNTER_LOCK: Mutex<()>` via `lock_gate_counters()`
  (`stream.rs:6738`) — held because the test reads and asserts on the
  `INTER_TX_TYPE_HITS` gate counters, which any concurrent gate-reading test
  would perturb.

It then asserts both `TxType::DctDct` and `TxType::Idtx` have non-zero hits,
decodes with our own decoder, and compares every luma sample against `ffmpeg`.

**What it waits on:** nothing but those two locks and the encode itself. The
lock order is `KNOBS` then `GATE_COUNTER_LOCK`, and I checked every other
`knob_write()` / `lock_gate_counters()` call site in the crate: they all take
the same order, so there is no ABBA cycle and no self-deadlock.

## 2. Two runs, alone, `--test-threads=1`, 20-minute cap

Both runs finished. The cap was never reached.

| run | wall | result |
|---|---|---|
| 1 | **50.56 s** | ok — `hits [73, 702, 13, 145, 66, 41, 405, 0, ...]`, 8211 bytes |
| 2 | **50.47 s** | ok — identical hits and byte count |

Run 1 was under `/usr/bin/time -v`: 99% CPU, `System 0.02 s`, 0 swaps,
max RSS 66 MB, 1005 involuntary context switches. Single-threaded, 99% CPU,
no child process, no I/O wait — the whole 50 s is compute in the encode.

**Two finishes: the wedge was not reproduced alone.** This is consistent with
the host-3 report that printed the same test as `ok`.

## 3. Stack capture — what actually wedges

`eu-stack` on the wedge. The test alone never wedges, so I reproduced the
*suite* condition: the full `encoder::tests` subset at `--test-threads=12`,
which held the process **past the 20-minute cap** (exit 124).

Thread states at capture (13 threads):

```
TID 1126828 (main)   S  std::test::run_tests_console
                        -> mpsc::list::Channel<test::event::CompletedTest>::recv
                        -> std::thread::park -> syscall            <-- futex_do_wait
TID 1126838          R  encode::Plane::trial_typed -> Plane::trial
                        -> encode::search_inter_block -> encode::search_tiles
                        -> encode::encode_inter_frame -> Av1Encoder::flush
                        -> encoder::tests::encode_all
                        -> encoder::tests::a_moving_detail_clip_codes_two_delta_q_levels_...
TID 1126839          S  RwLock::write_contended -> speed::knob_write
                        -> encoder::tests::a_non_superblock_aligned_clip_...
TID 1126840          S  RwLock::read_contended  -> speed::KNOBS
```

A later capture (the run still going) shows the convoy plainly — 5 of 12 test
threads parked, one working:

```
Thread 10  S  futex_wait -> RwLock::read_contended  (KNOBS)
Thread  9  S  futex_wait -> RwLock::read_contended  (KNOBS)
Thread  8  R  tile::write_coeffs -> msac::SymbolEncoder::symbol   <-- working
Thread  7  S  futex_wait -> RwLock::read_contended  (KNOBS)
Thread  6  S  futex_wait -> RwLock::write_contended (KNOBS)
Thread  5  S  futex_wait -> RwLock::read_contended  (KNOBS)
```

The convoy is reproducible on demand and its length is measurable. Two
independent runs of the `encoder::tests` subset at `--test-threads=12` were
both still going at **30 minutes** (one hit the 20-minute `timeout`, one hit a
30-minute wall) having completed 32 and 33 tests respectively. A final stack
at 30 min showed **7 of 9 threads parked** (`6 read_contended` +
`1 write_contended` on `KNOBS`) against a single `R` worker:

```
S S S S R S S S S      <-- one worker, eight parked
read_contended x6, write_contended x1
```

In BOTH subset runs the target test printed `ok`. It is a victim of the convoy
in the same way as the other `KNOBS` holders, not its origin.

This matches the wave14 signature exactly: **main in `futex_do_wait`, one
sibling `R` at 99% CPU**, no `aomenc`/`ffmpeg` child. The VPS saw 3 threads
rather than 13 because it ran the whole-crate suite at a lower thread count;
the mechanism is the same.

**The wait is named: `speed::KNOBS` (`RwLock<()>`), contended `write`/`read`.**
It is a convoy, not a deadlock. I sampled the `R` thread three times at 20 s
intervals and it was a *different* thread each time, always making forward
progress in real encode work (`mc::predict_with_filters_kern`, then
`tile::write_coeffs`, then `msac::update_cdf`):

```
sample 1  TID 1131366  R  mc::predict_with_filters_kern   (mc.rs:1341)
sample 2  TID 1128602  R  tile::write_coeffs               (tile.rs:6595)
sample 3  TID 1126841  R  msac::update_cdf                 (msac.rs:274)
```

A livelock or spin-wait would pin the same thread on the same frame forever.
Different threads advancing through different stages proves the run is
compute-bound and lock-serialized, not stuck. I also read `par.rs`: the worker
pool is condvar-based (`pool_worker` at `par.rs:293` blocks in
`inner.cv.wait(q)`, `par.rs:314`) with no spin loop, and the batch barrier at
`par.rs:485` uses `cv.wait`. There is no spin-wait in the pool.

Crucially: **in the reproduced wedge, the target test printed `ok`.** It is a
50 s holder of `KNOBS`, so on a loaded host the harness parks waiting on it and
libtest's "has been running for over 60 seconds" line — the last test printed —
is what the wedge report attributed to it. The attribution is a reporting
artifact of which test is last-printed, not a hang in this test.

## 4. Fix

None. The cause is not local to this test and not a lock defect:

- No deadlock: lock order is consistent crate-wide, verified at every
  `knob_write()` / `lock_gate_counters()` site.
- No spin-wait: `par.rs` is condvar-based.
- Not an infinite loop: the encode is 50 s and terminates, twice, identically.
- The test passes even inside the contended subset that hits the 20-min cap.

Shortening the convoy would mean making the long `KNOBS` holders cheaper or
excluding them from parallel runs, which is a suite-design decision across
many tests — out of scope here, and per the assignment I did not add an
`#[ignore]`.

### Open decision for Main (not actioned)

`an_inter_clip_codes_both_inter_set_tx_types_both_decoders_read_exactly`
(`encoder.rs:5415`) flips the same process-global lever as
`a_wide_tx_set_clip_codes_the_new_alphabets_both_decoders_read_exactly`
(`encoder.rs:5513`) and its third sibling (`encoder.rs:5637`). Both siblings
carry:

```rust
#[ignore = "sets process-global search levers: run it alone"]
```

The target does not. That asymmetry is the one thing that would actually
reduce suite wall time here, but it trades a passing un-ignored gate for an
ignored one, and the previous restage's skip was already rejected as "not a
fix". Left as a decision, not a change.

## Evidence / reproduction

```bash
git worktree add -b lane/av1intertxwedge ~/.cache/wt/av1intertxwedge origin/main
cd ~/.cache/wt/av1intertxwedge
RUSTC_WRAPPER= TMPDIR=$HOME/.cache/tmp CARGO_TARGET_DIR=$PWD/target \
  cargo test -p ec-av1 --lib --no-run

# alone, twice -- both finish in ~50 s
./target/debug/deps/ec_av1-79bef263d07a873f --exact \
  'encoder::tests::an_inter_clip_codes_both_inter_set_tx_types_both_decoders_read_exactly' \
  --test-threads=1 --nocapture

# the convoy: exceeds the 20-min cap, exit 124
timeout 1200 ./target/debug/deps/ec_av1-79bef263d07a873f 'encoder::tests' --test-threads=12
# stack while it is parked
eu-stack -p <pid>   # and: gdb -p <pid> -batch -ex "thread apply all bt 5"
```

Environment note: builds need `RUSTC_WRAPPER=` and `TMPDIR=$HOME/.cache/tmp` —
`/tmp` on this host is a 16 G tmpfs at 80% and sccache's temp files fail with
`Disk quota exceeded (os error 122)`. That is environmental, not a code result.

Also note: a relative-path edit from this worktree initially landed in the
PRIMARY checkout instead (`/home/tahinli/Documents/Code/Rust/edith_codecs`).
The 4 throwaway probe lines were reverted there and the primary is clean at
`1bac42c3`; all measurement edits after that used absolute worktree paths.
