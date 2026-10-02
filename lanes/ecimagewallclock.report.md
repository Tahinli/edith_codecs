# ec-image: replace the WebP budget test's wall clock with a byte bound

Lane: `lane/ecimagewallclock`. Touches one file, `crates/ec-image/tests/fuzz.rs`.
No shipped-crate change; the counting allocator lives in the test binary only.

## The failure

`a_webp_animation_with_many_tiny_frames_over_a_huge_canvas_is_refused_by_the_total_budget`
red on a VPS host with `3.894534671s` — the panic message *was* the duration,
because the assertion's own message was `"{elapsed:?}"`.

## What the assertion was protecting against

The decode is refused by `AllocBudget::spend` (`lib.rs:201`) the moment the
running total crosses `max_total_alloc` (4 GiB by default). What the test
protects is that the refusal happens *before* the work: `decode_frames` clones
the whole composited canvas once per ANMF chunk, so 64 tiny frames over a
16000x16000 canvas is 64 GiB of compositing if the guard stops firing.

So the property is **bytes of work performed before the refusal**, which is a
deterministic function of `max_total_alloc` and the canvas size. Wall time is
not that property. It is the host's memcpy bandwidth for a fixed ~4 GiB of
traffic, which is exactly the quantity that differed between the two hosts.

## Distribution on this workstation

Same case, `one_pixel_animated_webp(16000, 16000, 64)`, 15 runs each:

| build | min | median | max | raw (ms) |
|---|---|---|---|---|
| release | 0.747 s | 0.759 s | 0.893 s | 747 755 756 758 758 758 759 759 761 763 764 766 766 770 893 |
| debug | 0.774 s | 0.779 s | 0.821 s | 774 776 776 777 778 779 782 789 792 821 |
| VPS (reported) | — | — | **3.895 s** | — |

The desktop/VPS ratio is ~5x with byte-identical decoder work. A 2000 ms
ceiling calibrated on this host has ~2.5x of headroom over its own median —
it survives noise here and fails on any host slower than ~2.6x this one. The
existing 2000..3000 ms rerun escape hatch does not help: 3.895 s is outside
the rerun window, so the test took the slow path straight to the assertion.

The old 5 s ceiling on the GIF sibling was in the same class: measured here at
0.80 s median, i.e. also a proxy for memcpy bandwidth rather than for work.

## Decision

Bound the work in **bytes allocated**, measured with a thread-local counting
global allocator in the test binary, against the decoder's *own* limit
(`Limits::default().max_total_alloc`). No clock survives in these tests.

The ceiling is not a fitted constant — it is the budget the guard enforces, so
the assertion states the invariant directly: a decode that has refused at the
total budget cannot have allocated more than the total budget.

Two additional facts fall out of the byte measurement for free:

- **Size-relative.** The cost must not grow with the attacker's frame count, so
  each budget test decodes both a 64-frame and a 4096-frame file and bounds
  both. A guard spent per *input* frame, or after the clone instead of before
  it, is caught here; a clock cannot distinguish those, since the work is the
  same order of magnitude either way.
- **Red before the work, for cheap refusals.** The two header-refusal tests get
  a tight bound (64 KiB) expressing "refused before sizing anything"; measured
  421 B for the 60000x60000 GIF frame.

## Proof, both ways

### Green here

`cargo test -p ec-image` scoped, all three binaries:

- `fuzz`: 18 passed
- `differential`: 19 passed
- `real_library`: 2 passed

### Green under emulated slowness

The persistent shell wedged under the load generators, so the load ran as
detached `Bun.spawn` processes and the test binary was invoked directly.

Two mechanisms, because the decode is memcpy-bound and the VPS failure was a
memory-subsystem difference, not just a clock difference:

1. 16 CPU spinners on 12 cores + 6 `dd oflag=direct` memory hammers.
2. The same plus 24 spinners and 10 more hammers (1-minute load average 46).

Under mechanism 1, decode elapsed measured **1.46–1.75 s**; under mechanism 2,
**2.42 / 6.30 / 7.31 / 8.91 / 9.26 / 9.34 s**. That range straddles and then
clears the old 2000 ms ceiling, so the old assertion is reproduced failing
locally.

The new tests under that same load:

| test | result |
|---|---|
| webp budget | ok — 6.94 s, 9.44 s, 13.81 s |
| gif budget | ok — 6.39 s, 10.73 s |
| lzw bomb | ok — 0.03 s |
| gif pixels | ok — 0.00 s |

Green at wall times 3–7x past the point where the old ceiling went red, which
is the point: the assertion no longer reads the clock.

**What this covers:** contention, CPU starvation, and memory-bandwidth
pressure — the three mechanisms by which a host makes identical decoder work
take longer. **What it does not cover:** a genuinely slower *implementation*
(such as a build without memcpy optimizations), or hardware faults outside the
emulated axis. It does not need to: the byte bound is invariant to all of them
by construction, and the emulated runs exist to show the *old* bound was
host-dependent, not to certify the new one against hardware I do not have.

### Red against a genuine regression

The guard moved one line — `budget.spend` relocated from *before* the
`frames.push` clone to *after* it, so exactly one canvas clone too many happens
before the budget trips:

```
the refused decode allocated 5120024246 bytes, past the 4294967296-byte total budget
```

One extra 1 GiB canvas clone, and the test reds on the byte bound.

The same mutation measured through the old instrument: **1.006 s median
(1.004–1.010 s)**, comfortably *under* the 2000 ms ceiling — the clock
assertion would have passed a real regression. That is the measurement that
decides the question: the clock does not discriminate the regression it was
written for, and the byte bound does.

A second mutation (guard spent only on the first frame, so it never trips at
all) was killed by the memguard cgroup at 10 GiB — SIGKILL before any
assertion, i.e. unbounded work as intended.

### Non-vacuity

The refusal is still asserted on both inputs (`err` must contain `limit`), the
bound still discriminates a one-clone regression (above), and the 4096-frame
input is checked to be genuinely larger first, so the size-relative arm cannot
pass by comparing a file to itself.

## Class sweep

Same defect class (clock as a proxy for bounded decode work) elsewhere in the
same file, all converted on the same evidence:

| line | was | now | measured |
|---|---|---|---|
| webp budget | `< 2000 ms` + 2000..3000 rerun | `<= max_total_alloc` bytes, 64 and 4096 frames | 4.096 GB vs 4.295 GB |
| gif budget | `< 5 s` | `<= max_total_alloc` bytes, 64 and 4096 frames | 4.096 GB vs 4.295 GB |
| gif pixels | `< 100 ms` | `<= 64 KiB` bytes | 421 B |
| gif lzw bomb | `< 1 s` | `<= 1 MiB` bytes | far under; unbounded would be hundreds of MB |

`real_library.rs` also has clocks, but they are differential-decoder
comparisons that print timings rather than assert on them — not this class,
left alone.

## Implementation note

The byte tally is **per-thread**, not a global atomic. `cargo test` runs these
tests in parallel, and a shared counter summed concurrent decodes together —
the first attempt reported 6144529702 bytes for a decode that allocates
4096023126, because two tests' work was added together. Both budget tests
failed on that. Per-thread `Cell<u64>` counts only the calling thread's
allocations, and needs no arm/disarm flag.

## Notes

- Scratch probe (`crates/ec-image/examples/wallclock_probe.rs`) and the load
  script were removed; the diff is the test file only.
- An earlier pass of these edits leaked into the primary checkout via a
  relative path from the worktree cwd; reverted there with
  `git checkout -- crates/ec-image/tests/fuzz.rs` and redone against the
  absolute worktree path. The primary checkout is clean apart from the
  untracked `.wt/` directory.