# lane-av1enchang — `bitrate_target_lands_within_5_percent_over_48_frames`: a MEASUREMENT, not a hang

> **CORRECTION from the independent refutation pass `lanes/refute-av1-w3b.report.md` (2026-09-30).** The
> boxing and the tight-stack gate CONFIRMED — un-boxing `DpbSlot::cdfs` gives `fatal runtime error: stack
> overflow, aborting` (SIGABRT exit 101) as reported, and the size gate reds by name. ONE NUMBER IS STALE:
> `Av1Encoder` is **2,944 B** today, not 18,168 — 18,168 was correct when this merged, and `d202b8b4`
> boxed `carried_cdfs` the same day. Quote 2,944; the gate's red is
> `Av1Encoder is 124736 bytes, 15.23x over budget` = 2,944 + 7 x 15,232 (the eight DPB slots).


Branch `lane-av1enchang`, off `main` `3698787d`. One source file,
`crates/ec-av1/src/encoder.rs`, plus this report.

## Verdict

**The test is not hung, not stuck, and not a regression. It is a ~7 hour test in an
unoptimized build and it PASSES.** The "wedge" was a suite killed mid-test. Coordinator
evidence closes it: `/home/tCloud/gates/wave3e-suite.log` line 180 reads
`test encoder::tests::bitrate_target_lands_within_5_percent_over_48_frames ... ok`, and the
same log ends `test result: ok. 713 passed; 0 failed; 49 ignored; 0 measured; 0 filtered
out; finished in 24341.37s` — 6.76 hours for the whole `ec-av1` lib suite.

Independently reproduced here, and everything below is measured on this box, not inferred.

| question | answer | evidence |
|---|---|---|
| Does it terminate? | **yes** | all 8 arms pass, 4m40s optimized, on unmodified `main` |
| Is it a regression? | **no** | same test, same 4 bitrates, byte-identical output at pin `ea8fa18f` |
| Why so slow? | **not a pathology** | `evals_per_search` is flat at 61.3 → 64.9 as `q_idx` falls |
| Is the search degenerating? | **no** | 61.3/64.5/64.9 evals/search across 384k/1.5M/2M — that is the search's designed bound |
| Is the motion-search call count the defect? | **no** | 1,885–2,711 searches/frame over 60 superblocks = 31–45/superblock, bounded |

## What is landed

1. **A documented backstop**, valued from a measured run, so a future *non-terminating*
   encode fails this gate in 30 minutes instead of wedging a suite for hours. It is not
   presented as a fix for the cost, because it is not one.
2. **A real, separately-justified encoder fix**: `Av1Encoder` was **139,960 bytes by value**;
   it is now **18,168**. Two lines. It is a public-API stack hazard, unrelated to the wedge.
3. **A millisecond regression gate** for that fix,
   `the_pyramid_constructors_fit_on_a_tight_stack`: both constructors are built on a
   1 MiB stack, which the old layout cannot survive and the new one clears with 2.7x
   margin.

## Finding 1 — the cost, measured and attributed

### 1.1 Where the time goes: the motion-search census

`motion.rs` already keeps a census (`searches`, `evals`, `subpel evals`,
`zero_mv`). A temporary probe (`tmp_mv_census`, since removed) read it per arm, optimized,
48 frames of 640x384, flat path:

| arm | searches | evals | **evals/search** | evals/frame | s/frame | searches/frame |
|---|---|---|---|---|---|---|
| 384 000 bps (`q` walks **up**) | 90,495 | 5,547,925 | **61.3** | 115,582 | 0.53 | 1,885 |
| 1 536 000 bps (`q` → ~25) | 122,011 | 7,869,268 | **64.5** | 163,943 | 0.90 | 2,542 |
| 2 000 000 bps (`q` lower still) | 130,163 | 8,447,516 | **64.9** | 175,990 | 1.02 | 2,712 |

`evals_per_search` is **flat**. A diamond log-step search over 8 neighbours plus the
half-pel and quarter-pel refinement stages has a designed bound right around 56–64 evals,
and the measured 61.3 → 64.9 sits on it. **The search is not degenerating and there is no
runaway call count to bound.** The cost is the plain product

> full search x 60 superblocks (640/64 x 384/64) x 48 frames x 8 arms x ~13x debug penalty

which is why the expensive arms are the **high-bitrate** ones: the controller must walk
`q_idx` *down* from its seed 100 into the 20s to spend an 8000-byte budget, and a
lower-`q` frame costs 1.4-1.5x more (163,943 vs 115,582 evals/frame). At 384k/768k the
loop pushes `q` *up* and those arms are cheap. That is why the first four arms of a
partially-completed run print and the fifth does not.

### 1.2 Why the controller walks `q` down at all

At its seed `base_q_idx` of 100 this clip codes **5467 bytes** for the key frame, and the
`1_536_000 bps` target is `1536000/8/24 = 8000` bytes/frame — the seed is *already under
budget*, so the loop must lower `q_idx` at `STEP_CLAMP`=12 per frame to spend the rest.
Per-frame probe, one arm, optimized:

```
TMP frame  7 at 0.0s (+0.00s) q=100 bytes_so_far=0
TMP frame  8 at 0.8s (+0.78s) q=75  bytes_so_far=5467
TMP frame  9 at 1.3s (+0.53s) q=63  bytes_so_far=9725
TMP frame 11 at 2.9s (+0.83s) q=39  bytes_so_far=20520
TMP frame 20 at 11.5s (+1.04s) q=32 bytes_so_far=91906
TMP frame 30 at 20.5s (+0.88s) q=23 bytes_so_far=171230
TMP frame 47 at 35.9s (+0.81s) q=27 bytes_so_far=308242
TMP done bps=1536000 pyr=false coded=372465
```

`q_idx` settles into a 22..36 band and **stops walking**. The same arm debug: `+10.79s,
+8.61s, +11.24s, +12.18s` at the same `q` — the ~13x profile ratio.

### 1.3 Three gdb samples prove the thread is walking, not repeating

Taken 20 s apart on the `1_536_000 bps` flat arm (the arm a short run is inside when it
looks wedged):

```
$ for i in 1 2 3; do gdb -p <pid> -batch -ex 'thread apply all bt'; sleep 20; done
```

Sample 1 (08:47:35) — MV-search / transform, block `x=192,y=80`, `base_q_idx=32`:

```
#8  ec_av1::transform::permute<i32> (t=0x7f9809f91490, n=4) at crates/ec-av1/src/transform.rs:355
#9  ec_av1::transform::inverse_dct_n<i32, 4> (t=0x7f9809f91490, r=16) at crates/ec-av1/src/transform.rs:413
#11 ec_av1::transform::inverse_1d (t=0x7f9809f91490, log2=4, r=16, kind=ec_av1::transform::TxType1d::Dct) at crates/ec-av1/src/transform.rs:1103
#15 ec_av1::transform::inverse_transform_2d_typed_wh (dequant=..., w=16, h=16, bit_depth=8, tx_type=ec_av1::transform::TxType::DctDct) at crates/ec-av1/src/transform.rs:1171
#19 ec_av1::transform::dequant_and_inverse_typed_wh (levels=..., w=16, h=16, bit_depth=8, q_idx=32, dc_delta=0, ac_delta=0, tx_type=…DctDct, qm=…) at crates/ec-av1/src/transform.rs:1375
#20 ec_av1::encode::Plane::code_from_prediction_typed (self=0x7f9809f9c460, x=192, y=80, side=16, prediction=..., skip=false, base_q_idx=32, deadzone=0.5, set=ec_av1::cdf_state::TxbSet::Chroma16, tx_type=…DctDct) at crates/ec-av1/src/encode.rs:3109
#22 ec_av1::encode::mc_trial_compound (plane=0x7f9809f9c460, x=192, y=80, side=16, luma=false, ref0=..., ref1=..., base_q_idx=32, …) at crates/ec-av1/src/encode.rs:10435
#23 ec_av1::encode::search_inter_block (luma=0x7f9809f9c340, chroma=0x7f9809f9c3d0, search=0x7f9809f9eb28, mode_bits=0x7f9809fa6998, reference=0x7f980a01e6b0, stack=0x7f9809f9ec10, extra=..., compound=..., grid=0x7f9809f9c610, fctx=0x7f980a0403d0) at crates/ec-av1/src/encode.rs:12331
#24 ec_av1::encode::encode_inter_frame::{closure#19} (index=0, fctx=0x7f980a01e6b0) at crates/ec-av1/src/encode.rs:14673
#25 ec_av1::encode::search_tiles::{closure#0} (index=0) at crates/ec-av1/src/encode.rs:8704
```

Sample 2 (08:47:55) — a different function and a different block:

```
#5  ec_av1::encode::intra_predict_u8::{closure#0} (src=..., buf=0x7f9809f90aee) at crates/ec-av1/src/encode.rs:1691
#6  ec_av1::encode::intra_predict_u8 (mode=2, angle_delta=0, above=..., left=..., corner=..., bw=8, bh=8, enable_edge_filter=false, smooth_neighbor=false, dst=..., fctx=0x7f980a0403d0) at crates/ec-av1/src/encode.rs:1696
#7  ec_av1::encode::{impl#2}::search_block::{closure#0} () at crates/ec-av1/src/encode.rs:3564
#18 ec_av1::encode::Plane::search_block (self=0x7f9809f9c340, at=..., search=0x7f9809f9eb28, mode_bits=0x7f9809fa6998, fctx=0x7f980a0403d0) at crates/ec-av1/src/encode.rs:3588
#19 ec_av1::encode::code_square (luma=0x7f9809f9c340, chroma=0x7f9809f9c3d0, side=8, search=0x7f9809f9eb28, mode_bits=0x7f9809fa6998, tx_select=true, ibc=..., census_key=false, fctx=0x7f980a0403d0) at crates/ec-av1/src/encode.rs:6271
#20 ec_av1::encode::code_square_inter (luma=0x7f9809f9c340, chroma=0x7f9809f9c3d0, side=8, search=0x7f9809f9eb28, mode_bits=0x7f9809fa6998, reference=0x7f980a01e6b0, stack=0x7f9809f9a550, compound=..., grid=0x7f9809f9c610, refs=0x7f9809f9c698, fctx=0x7f980a0403d0) at crates/ec-av1/src/encode.rs:7266
#25 ec_av1::encode::encode_inter_frame::{closure#19} (index=0, fctx=0x7f980a0403d0) at crates/ec-av1/src/encode.rs:14724
```

Sample 3 (08:48:16) — a third block, and a **later frame's** quantizer:

```
#1  ec_av1::tile::base_ctx (grid=..., side=32, row=6, col=30, class=ec_av1::decode::TxClass::TwoD) at crates/ec-av1/src/tile.rs:6759
#14 ec_av1::tile::coeff_bits_typed (grid=..., set=ec_av1::cdf_state::TxbSet::Luma32Inter, q_ctx=1, skip_ctx=0, sign_ctx=1, tx_type=…DctDct) at crates/ec-av1/src/tile.rs:6004
#15 ec_av1::tile::rdoq::price (levels=..., dense=..., side=32, coded=32, set=…Luma32Inter, q_ctx=1, skip_ctx=0, sign_ctx=1, tx_type=…DctDct) at crates/ec-av1/src/tile.rs:6116
#16 ec_av1::tile::rdoq (levels=..., scaled=..., side=32, base_q_idx=30, set=…Luma32Inter, skip_ctx=0, sign_ctx=1, lambda=0.060500000000000005, tx_type=…DctDct) at crates/ec-av1/src/tile.rs:6176
#17 ec_av1::encode::Plane::code_from_prediction_typed (self=0x7f9809f9c340, x=256, y=128, side=32, prediction=..., skip=false, base_q_idx=30, deadzone=0.5, set=…Luma32Inter, tx_type=…DctDct) at crates/ec-av1/src/encode.rs:3083
#19 ec_av1::encode::mc_trial (plane=0x7f9809f9c340, x=256, y=128, side=32, mv=..., luma=true, reference=..., stride=640, ref_width=640, ref_height=384, skip=false, base_q_idx=30, …) at crates/ec-av1/src/encode.rs:10341
#20 ec_av1::encode::search_inter_block (luma=0x7f9809f9c340, chroma=0x7f9809f9c3d0, search=0x7f9809f9eb28, mode_bits=0x7f9809fa6998, reference=0x7f980a01e6b0, stack=0x7f9809f9ec10, extra=..., compound=..., grid=0x7f9809f9c610, fctx=0x7f980a0403d0) at crates/ec-av1/src/encode.rs:12407
```

Three samples, three functions, three blocks, and `base_q_idx` **32, 32, 30** — the `30`
can only be a *later frame's*, because `RateLoop::update` runs once per coded frame. This
is consistent with, and independently confirms, the coordinator's own 100%-CPU
`search_root_128_rect -> search_skip_64 -> motion::search` reading.

### 1.4 Not a regression

Same test, same four bitrates, run at the older pin in a detached worktree:

```
$ git worktree add /home/tahinli/.cache/wt/av1pin ea8fa18f --detach
$ cargo test -p ec-av1 --release --lib -- bitrate_target_lands_within_5_percent_over_48_frames
bitrate 384000 bps, pyramid false: 94646 bytes over 2.00s = 378584 bps (-1.4%) | … leaf 47x1897
bitrate 384000 bps, pyramid true:  97738 bytes over 2.00s = 390952 bps (+1.8%) | … leaf 35x648
bitrate 768000 bps, pyramid false: 187877 bytes over 2.00s = 751508 bps (-2.1%) | … leaf 47x3881
bitrate 768000 bps, pyramid true:  191961 bytes over 2.00s = 767844 bps (-0.0%) | … leaf 35x1536
bitrate 1536000 bps, pyramid false: 372465 bytes over 2.00s = 1489860 bps (-3.0%) | … leaf 47x7808
bitrate 1536000 bps, pyramid true:  384161 bytes over 2.00s = 1536644 bps (+0.0%) | … leaf 35x3076
bitrate 2000000 bps, pyramid false: 491321 bytes over 2.00s = 1965284 bps (-1.7%) | … leaf 47x10337
bitrate 2000000 bps, pyramid true:  500299 bytes over 2.00s = 2001196 bps (+0.1%) | … leaf 35x6394
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 782 filtered out; finished in 275.53s
real	4m35.622s
```

**Byte-identical to current `main`**, arm for arm. Nothing changed; the gate has always
been this expensive.

### 1.5 Reproduce, for the record

```
$ export CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1enchang2 EC_NOMEMGUARD=1 \
         EC_FIXTURES=/home/tahinli/Documents/Code/Rust/edith_codecs/fixtures \
         EC_REQUIRE_FIXTURES=1
$ timeout 1800 cargo test -p ec-av1 --lib -- bitrate_target_lands_within_5_percent_over_48_frames \
      --test-threads=1 --nocapture
running 1 test
test encoder::tests::bitrate_target_lands_within_5_percent_over_48_frames ...
bitrate 384000 bps, pyramid false: 94646 bytes … (-1.4%)
bitrate 384000 bps, pyramid true:  97738 bytes … (+1.8%)
bitrate 768000 bps, pyramid false: 187877 bytes … (-2.1%)
bitrate 768000 bps, pyramid true:  191961 bytes … (-0.0%)
EXIT=124          <-- 4 of 8 arms in 30 minutes; the fifth is simply still running
```

Two traps worth recording, both of which produce a false "it hung" reading:

* **Without a fixture root the gate does not run and libtest still says `ok`**:
  `test … SKIP …: h264-1080p-23.976-8bit.mp4 absent at …` / `test result: ok. 1 passed …
  finished in 0.06s`.
* **Without `--nocapture`, libtest buffers the per-arm `eprintln!` lines**, so a run that
  has done real work prints only the test name. That is what a wedge looks like in a log.

## Finding 2 — the landed fix: a backstop, valued from a measured run

The gate had no way to *fail*. A future non-terminating encode would wedge a suite for
hours with no diagnostic — which is how wave3j and wave3h were lost. So each arm's encode
now runs on a **worker thread** and the test thread watches a **per-CALL** deadline over an
`mpsc` channel: the worker sends a progress ticket after every `encode_frames` call and
its result after `flush` (which is itself a call that can spin, so it gets a ticket too,
not a bare `join` that would block forever). On timeout the gate panics.

* `ArmResult` (targets/coded/bytes/count) carries the tally off the worker.
* `pictures` is an `Arc`, not an 8x 35 MB clone.
* Worker is a plain `std::thread::spawn` — the std default 2 MiB stack. libtest spawns
  its test thread with `thread::Builder` and **no** `.stack_size()`, so the encode gets a
  byte-identical stack to before: the backstop neither introduces nor masks a stack
  problem.

### The budget, and its margin

**The budget is per call, not per test** — that distinction is the whole reason this shape
is safe. An earlier 300 s value was wrong for the pyramid arms and has been raised to
**1800 s**.

| | worst single `encode_frames` call | budget | margin |
|---|---|---|---|
| optimized, flat arms | **1.04 s** (measured, frame 20 at `q_idx` 32) | 1800 s | **1730x** |
| debug, flat arms | **12.2 s** (measured) | 1800 s | **148x** |
| debug, pyramid arms | **~110 s** (derived: a pyramid call codes a whole mini-GOP, ~9 frames, at 12.2 s each) | 1800 s | **~16x** |

Whole-test wall time for reference: **280.60 s optimized** for all eight arms on unmodified
`main` (`finished in 280.60s`, `real 4m40.700s`), 275.53 s at `ea8fa18f`, 311.29 s on this
branch. A host ~16x slower than this workstation still clears a 1800 s per-call budget, so
the default cannot turn a correct-but-slow build red.

### The red, both directions

**Red 1 — the timeout path is live on the real workload** (budget forced under the honest
cost, so it fires inside a healthy run):

```
$ EC_AV1_ENCODE_CALL_BUDGET_SECS=1 … --nocapture
thread 'encoder::tests::bitrate_target_lands_within_5_percent_over_48_frames' panicked at crates/ec-av1/src/encoder.rs:3551:76:
AV1 bitrate gate SLOW, NOT HUNG: the 384000 bps pyramid false arm's encode did not return
within 1s (arm elapsed 22s, stuck on call 49 of 49 -- the call that never came back). …
FAILED
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 788 filtered out; finished in 21.79s
EXIT=101
```

It fails loudly and never as `ok`: the test name in the panic, the arm name, the arm's
elapsed seconds, the stuck call index, `FAILED`, and `EXIT=101`.

**Red 2 — a genuine non-terminating encode at the default budget.** A
`loop { std::hint::spin_loop(); }` injected at the top of `Av1Encoder::encode_frames`
(temporary; the tree now greps **0** hits for `EC_AV1_INJECTED_SPIN`):

```
$ EC_AV1_INJECTED_SPIN=1 … --nocapture
thread '…' panicked at crates/ec-av1/src/encoder.rs:3535:76:
AV1 encode did not return within 300s, on call 1 of 49 (384000 bps pyramid false). …
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 788 filtered out; finished in 300.16s
real	5m0.249s
user	9m59.831s
EXIT=101
```

(The 300 s in that transcript is the budget value at the time of the run; the default is
now 1800 s and the same injection still fires.) Pre-fix, that input wedges the suite
forever; post-fix it is a bounded RED. `user 9m59s` against `real 5m0s` is the two-CPU
burn signature the wedged hosts showed.

**Green**, default budget, all eight arms, **byte-identical to the pre-fix run**:

```
bitrate 384000 bps, pyramid false: 94646 bytes … (-1.4%)
bitrate 384000 bps, pyramid true:  97738 bytes … (+1.8%)
bitrate 768000 bps, pyramid false: 187877 bytes … (-2.1%)
bitrate 768000 bps, pyramid true:  191961 bytes … (-0.0%)
bitrate 1536000 bps, pyramid false: 372465 bytes … (-3.0%)
bitrate 1536000 bps, pyramid true:  384161 bytes … (+0.0%)
bitrate 2000000 bps, pyramid false: 491321 bytes … (-1.7%)
bitrate 2000000 bps, pyramid true:  500299 bytes … (+0.1%)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 788 filtered out; finished in 311.29s
EXIT=0
```

## Finding 3 — a 137 KB `Av1Encoder` aborts on a default-sized stack

**This is a separate defect and is NOT the fix for the wedge.** It is landed on its own
merits. Reproduced twice on the pre-fix binary built from unmodified `main`, 6m18s and
6m35s, by invoking the test binary **directly** rather than through `cargo test`:

```
$ /home/tahinli/.cache/tgt/av1enchang/debug/deps/ec_av1-79bef263d07a873f \
    bitrate_target_lands_within_5_percent_over_48_frames --test-threads=1 --nocapture
test … bitrate 384000 bps, pyramid false: 94646 bytes … (-1.4%)
thread 'encoder::tests::bitrate_target_lands_within_5_percent_over_48_frames' (2394233) has overflowed its stack
fatal runtime error: stack overflow, aborting
real	6m35.006s
```

Not the encode — the **constructor**:

```
$ coredumpctl debug 2394232
Thread 1 (Thread 0x7f00483d66c0 (LWP 2394233)):
#6  <signal handler called>
#8  ec_av1::encoder::Av1Encoder::new (config=...) at crates/ec-av1/src/encoder.rs:1519
#9  ec_av1::encoder::Av1Encoder::with_pyramid (config=..., pyramid=...) at crates/ec-av1/src/encoder.rs:1583
#10 ec_av1::encoder::Av1Encoder::with_pyramid_and_rate_target (config=..., pyramid=..., rate=...) at crates/ec-av1/src/encoder.rs:1615
#11 ec_av1::encoder::tests::bitrate_target_lands_within_5_percent_over_48_frames () at crates/ec-av1/src/encoder.rs:3427
```

### 3.1 The regression gate, and the measured threshold sweep

Proving the old layout aborts by re-running the 48-frame gate takes 6m35s and needs a
process that dies. A millisecond gate says the same thing: build **both** constructors on a
thread with a deliberately tight stack. New test
`the_pyramid_constructors_fit_on_a_tight_stack` does exactly that, and both thresholds were
swept in an unoptimized build (the Box temporarily reverted to get the pre-fix column):

| stack | pre-fix (`CdfSnapshot` by value) | post-fix (boxed) |
|---|---|---|
| 256 KiB | overflow | overflow |
| 384 KiB | overflow | **ok** |
| 512 KiB | overflow | ok |
| **1 MiB** | **overflow** | **ok** |
| 2 MiB (libtest's default) | **overflow** | ok |
| 3 MiB | ok | ok |

The gate runs at **1 MiB**: 2.7x what the boxed layout needs and 2x below what the old one
did, so it is nowhere near either threshold and cannot flake. The pre-fix row at 1 MiB and
at 2 MiB is the red.

```
$ EC_AV1_TIGHT_STACK_BYTES=1048576 … the_pyramid_constructors_fit_on_a_tight_stack --nocapture
TIGHTSTACK 1048576 bytes: Av1Encoder=18168 bytes, both constructors returned
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 789 filtered out; finished in 0.00s
```

Sizes, measured:

```
BEFORE: SIZE Av1Encoder=139960 Result<Av1Encoder>=139960
AFTER:  SIZE Av1Encoder=18168  Result<Av1Encoder>=18168
```

The 137 KB is `dpb: [Option<DpbSlot>; 8]`, each `DpbSlot` holding a 15 KB `CdfSnapshot` by
value. `Av1Encoder` is returned by value through three nested `Result<Self>` constructors
(`with_pyramid_and_rate_target` -> `with_pyramid` -> `new`), so an unoptimized build puts
several 137 KB copies on the stack at once. The flat path nests one level fewer and fits.

### Why `cargo test` never sees it, and why it is still real

`.cargo/config.toml` already carries the mitigation, and its comment already names this
exact failure:

```toml
[env]
# The AV1 rate-target tests keep several frames' worth of encoder state on the
# test thread's stack; the 2 MB default overflows in debug builds (release
# passes). Cap lives in the repo so every checkout and worktree gets it.
RUST_MIN_STACK = "67108864"
```

`cargo test` hands the test thread 64 MiB, so the abort does not happen there. **This is
also why the `cargo test` runs wedge rather than abort**, and it is the direct answer to
the `memguard-runner.sh` question: that runner is a **cgroup `MemoryMax=10G` /
`MemorySwapMax=2G` cap, not `RLIMIT_AS`**, and the test uses far less than 10 GB. It cannot
turn a guard-page hit into a sustained hang. A guard-page hit aborts; it does not sit in
`R` for three hours.

The defect is still real: `RUST_MIN_STACK` is a `[env]` entry that applies only to **this
workspace's** `cargo` invocations. Any external consumer linking `ec-av1` and calling
`Av1Encoder::new(...)` on an ordinary 2 MiB thread stack **crashes the process**. The repo
is hiding a public-API hazard behind a workspace-local env var.

### The fix, and its evidence

`DpbSlot::cdfs` becomes `Box<crate::encode::CdfSnapshot>`; the one construction site
(`refresh()`) wraps it in `Box::new`. Two lines.

```
$ cargo test -p ec-av1 --release --lib -- round_trip encode
test result: ok. 108 passed; 0 failed; 40 ignored; 0 measured; 641 filtered out; finished in 503.15s
```

**108 passed, 0 failed** — re-run *after* the change, not asserted. `size_of` before/after
is above. `cargo check -p ec-av1 --all-targets` is clean with no warnings. Nothing in the
suite was red for an unrelated reason.

## Commands

```
git -C /home/tahinli/Documents/Code/Rust/edith_codecs worktree add \
    /home/tahinli/.cache/wt/av1enchang -b lane-av1enchang main
git -C /home/tahinli/Documents/Code/Rust/edith_codecs worktree add \
    /home/tahinli/.cache/wt/av1pin ea8fa18f --detach          # the not-a-regression control
export EC_FIXTURES=/home/tahinli/Documents/Code/Rust/edith_codecs/fixtures
export EC_REQUIRE_FIXTURES=1

cargo check -p ec-av1 --all-targets
cargo test -p ec-av1 --lib      -- bitrate_target_lands_within_5_percent_over_48_frames --test-threads=1 --nocapture
cargo test -p ec-av1 --release --lib -- bitrate_target_lands_within_5_percent_over_48_frames --test-threads=1 --nocapture
EC_AV1_ENCODE_CALL_BUDGET_SECS=1  …   # red 1
EC_AV1_INJECTED_SPIN=1            …   # red 2 (injection reverted, 0 grep hits)
coredumpctl debug <pid>                # the Finding 3 backtrace
cargo test -p ec-av1 --release --lib -- round_trip encode
```

## Decision wanted from the coordinator (not taken here)

The honest cost is now measured and the gate is bounded, but **the ~7 hour debug suite
remains**. That is a test-design call, and it changes what the default suite proves, so it
is not this lane's to make:

* **leave it** — the claim stays fully in the default debug suite; gate scheduling must
  budget ~7 h (measured 24341 s on the fleet).
* **`#[ignore]` with a stated reason, run in release** — 4m40s instead of hours, at the cost
  of the claim no longer running by default.
* **split it** — e.g. a cheap 2-bitrate/short-clip arm in the default suite plus the full
  4x2x48 sweep behind a flag or a release-only gate.

Recommendation: **split**. It keeps a rate-control regression signal in the default debug
suite at a cost the suite can absorb, and keeps the full ±5% sweep available where it can
be afforded. Not implemented here — it changes what the gate proves, which is the
coordinator's call.
