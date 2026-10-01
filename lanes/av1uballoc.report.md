# lane/av1uballoc — the plane allocation closure, decided with a number

Started from base `2d8fa8a4`; **rebased onto `f01e9738` and re-measured there**
(§7). Target: `crates/ec-av1/src/decode.rs`'s `fresh_plane` and the
`unsafe { v.set_len(n) }` behind it.

**Decision: ship closure (b), `vec![0u16; n]` (`alloc_zeroed`/`calloc`).** It
costs **+0.84%** of decode wall time at 3840x2160 4:2:0 and **+0.81%** at
3840x2160 4:2:2 (both ~6σ against a 6–10 ms run-to-run spread), and **−1.63%**
at 1920x1080 — it is *faster* there, reproducibly. It removes the `unsafe`
from every build that is not explicitly asking for a census arm.

**No fixture on current main reaches an uninitialised read.** The reads-before-
write census below found zero, by two independent instruments, over all 128
committed fixtures. The charter's stop condition did not fire.

---

## 0. Two corrections to the charter before the numbers

* **`EC_AV1_PERF*` does not exist** in this repository — not in `crates/ec-av1`,
  not anywhere. The real decode-timing instruments are `EC_AV1_TIMELINE`
  (`src/timeline.rs:1-13`, dumped by `stream.rs:1369`, read by
  `scripts/av1-timeline-report.py`) and `EC_AV1_WAVE_STATS`. Neither buckets
  decode into stages — they bucket a *frame's phases*, and the report script's
  own doc says the per-phase sums exceed `wall` by construction, so they must
  not be summed. Neither attributes a cost to an allocation, so this lane added
  `examples/alloc_timing.rs`, which times the `decode_stream_with` call and
  reads `minflt`/`majflt` from `/proc/self/stat` around it. It prints one
  machine-readable line per process, which is what makes the interleaved
  round-robin below possible.

The closures are an **env arm inside the crate** (`EC_AV1_PLANE_ALLOC`), not a
build flag, so all three share one binary, one allocator state and one machine.
Three separate builds cannot make this comparison fair — the recycled-arena
state, the page-cache state and the code layout all differ between them.

---

## 1. The table

`cargo run -p ec-av1 --release --example alloc_timing -- <stream>`, arms
round-robined across processes so a drift hits every arm equally. Medians of 11
runs each (`n=11` below). Linux 7.2.7, 12 cores, glibc, default allocator,
single-threaded decode (`EC_AV1_THREADS=1` is the crate default).

### 3840x2160 4:2:0, 96 frames

| arm | median ms | min ms | mean ms | stdev ms | Δ% | minflt | output hash |
|---|---|---|---|---|---|---|---|
| `uninit` — status quo, `set_len` | 6937.728 | 6926.888 | 6939.318 | 7.731 | +0.00 | 280 846 | `c93abb392857618d` |
| `fill` — closure (a), explicit zero-fill | 7001.420 | 6992.227 | 7001.984 | 6.448 | **+0.92** | 215 336 | `c93abb392857618d` |
| `zeroed` — closure (b), `alloc_zeroed` | 6995.727 | 6978.615 | 6995.769 | 10.201 | **+0.84** | 278 704 | `c93abb392857618d` |

### 3840x2160 4:2:2, 48 frames

| arm | median ms | min ms | mean ms | stdev ms | Δ% | minflt | output hash |
|---|---|---|---|---|---|---|---|
| `uninit` | 4709.657 | 4703.387 | 4716.928 | 16.443 | +0.00 | 204 348 | `1c8526888dc2285d` |
| `fill` | 4750.184 | 4741.289 | 4751.697 | 7.264 | **+0.86** | 142 987 | `1c8526888dc2285d` |
| `zeroed` | 4747.944 | 4740.307 | 4749.023 | 7.219 | **+0.81** | 202 410 | `1c8526888dc2285d` |

### 1920x1080 4:2:0, 96 frames

| arm | median ms | min ms | mean ms | stdev ms | Δ% | minflt | output hash |
|---|---|---|---|---|---|---|---|
| `uninit` | 1768.411 | 1767.167 | 1775.918 | 9.964 | +0.00 | 60 023 | `ba5f2444c2b60885` |
| `fill` | 1735.265 | 1731.097 | 1742.735 | 21.224 | **−1.87** | 47 018 | `ba5f2444c2b60885` |
| `zeroed` | 1739.516 | 1733.861 | 1743.644 | 10.109 | **−1.63** | 59 170 | `ba5f2444c2b60885` |

**Every hash is identical within a stream.** The closure chosen moves not one
output sample.

### What the table says, and what it does not

* **(b) is not the free lunch the arm's name advertises.** `zeroed` and `fill`
  land within 0.03 % of each other at every size. glibc hands a *recycled*
  arena block back from `calloc` with the same memset an explicit fill does; the
  "a page fault per page is the mechanism" story is wrong for a decoder that
  frees and re-allocates the same plane every frame.
* **The cost is one extra pass over a buffer the decode immediately overwrites**:
  +55 ms over 96 frames of 24.9 MB ≈ 42 GB/s of extra write bandwidth. The
  `minflt` column is *not* the story — `fill` faults the fewest pages at 4K
  (215 k vs 281 k) and is still the slowest arm, so page-fault count does not
  explain the delta.
* **The 1080p sign flip is real and reproducible** (two independent 11-run
  rounds: −1.59 %/−1.51 %, then −1.87 %/−1.63 %) and **is not explained here**.
  I am reporting the number, not a story for it. It is the one result a
  reviewer should re-run before trusting the 4K cost on their own hardware.

---

## 2. Reads-before-write census — regions × readers × reachability

### 2.1 The two regions

`fresh_plane(n, true_width, true_height, width, height)` hands back `n =
width * height` slots. `width` is `block_grid(mi_cols) * 32` and
`true_width = mi_cols * 4`, so the plane is a **block-aligned coding surface**
with up to 31 px of margin per axis. `true_width`/`true_height` is the decodable
extent. That splits the allocation into two disjoint regions that can hold a
sample no block wrote:

| # | region | definition | can a reader reach it? |
|---|---|---|---|
| 1 | **padded tail** | everything outside `true_width × true_height`, out to `width × height` | in principle; **not in practice** — see §2.3 |
| 2 | **true extent** | inside `true_width × true_height` that the tile walk skipped | yes — this is where the withdrawn 224-sample demonstration sat |

The old `census_unwritten` scanned region 2 only, under a doc comment asserting
*"past that is the padded coding surface's invented tail, which no decoder ever
reads"*. That assertion is exactly what an unread tail looks like, so it was
self-certifying. This lane scans both and counts them separately
(`PAD_UNWRITTEN_SAMPLES` / `take_pad_unwritten_samples`), and the comment now
says what was measured.

### 2.2 The readers

Line numbers are `main` = `2d8fa8a4`.

| reader class | sites | reads the CURRENT frame? | clamps to true extent? | reachable on a current fixture? |
|---|---|---|---|---|
| intra prediction (neighbour edges, above/right/below/left) | `PlaneBuf::above/left/below/right_of`, `decode.rs:22059-22110` (`own_across = x + side.min(true_width - x)`, same down) | yes | **yes**, `saturating_sub(true_width/true_height)` plus `tile_x1/tile_y1` | no |
| intra TX bound checks | `decode.rs:12630, 13007, 15179, 19411, 23202, 23736, 24428, 25031, 39153, 39341` (`if px >= true_width { continue }`) | yes | yes | no |
| CfL (chroma-from-luma) | via the TX store paths above | yes (luma block region) | yes | no |
| IntraBC / intrabc copy | `decode.rs:14451-14527, 15009-15074` | yes (the block's own luma) | yes | no |
| intra-in-inter | `decode.rs:18266-18387, 23062-23095` | yes | yes | no |
| deblock | `apply_deblock`, `decode.rs:33367-33370` (`tw = true_width.min(cw)`) | yes | yes | no |
| CDEF | `cdef_gather(..., true_width, true_height, ...)`, `decode.rs:34397-34484` | yes | yes | no |
| loop restoration | `restoration.rs:1053+`, driven with `true_width`/`true_height` (`decode.rs:35315-35319`) | yes | yes | no |
| superres upscale | `superres.rs:100-240`, over the mi-aligned margin, cropped to `true_width` | yes | yes | no |
| **inter prediction / MC / warp / OBMC / compound** | `decode.rs:40384-40386, 40399-40406, 42757-42761, 44560-44581` (all pass `true_width`/`true_height` of the REFERENCE plane) | **no — a different frame's buffer** | n/a | n/a |
| reference-frame materialisation | the crop at `decode.rs:38480-38548` | copies the true extent only | yes | n/a |
| `pix_write`'s `prev` argument — **the one unconditional read-before-write in the shipped source** | `decode.rs:22073` (fn), called at `22294`, `22450`, `39014`, `39029`, passing `*o` / the slot's own value | yes — reads the destination sample *before* overwriting it | no | **fires on every store, on every fixture — but is not observable.** See §2.5 |

The load-bearing structural fact: **every current-frame reader clamps to
`true_width`/`true_height`**, which is why region 1 never fires. The 224-sample
defect of `lanes/unwritten-dep.report.md` was in region 2 and needed no
clamp-bypass — it was the tile walk skipping samples, not a reader reaching
past the edge.

### 2.3 Region census — measurement

`EC_AV1_PLANE_SENTINEL=1 decode_probe <fixture>`, summed over all 128
committed fixtures, both regions, at the **pre-deblock** point
(`census_unwritten`, called at `decode.rs:38139` in
`decode_key_frame_tile_with_cdfs` and `56507` in the inter path — after
`flush_recon`, before `apply_deblock`):

```
TOTAL  extent_scanned=1,466,581,280  EXT_HOLE=0
       pad_scanned  =  22,787,296   PAD_HOLE=4,676,128
fixtures with a nonzero hole in either region: 38
```

* **Region 2 (true extent): 0 unwritten of 1.47 G scanned.** The withdrawn
  defect is gone from the current corpus, and the one fixture that
  demonstrated it is refused:
  `440_request_is_422` → `REFUSED: unsupported: AV1 tile (a block size 4x8,
  8x16 or 16x4 (or 8x4 at 4:4:0) has no chroma plane block at this frame's
  subsampling mode …)` (`decode.rs:21643`).
* **Region 1 (padded tail): 4.68 M unwritten of 22.8 M scanned (20.5 %)** on
  38 fixtures — unwritten *by construction*, since no block lands there. This
  is the region whose "nobody reads it" claim had never been measured.
* On the generated 3840x2160 4:2:2 stream the tail is non-zero on the first
  two frames and zero after (`pad_unwritten=4096/1024/1024` on frame 0,
  `2048/512/512` on frame 1) out of 61 440/15 360/15 360 scanned per frame —
  so a 4:2:2 frame does leave real unwritten tail behind at scale.

### 2.4 Read census — measurement, and its non-vacuity

The region census counts **unwritten** samples. It says nothing about reads,
which is the whole question. Two independent read instruments:

**(a) valgrind memcheck, differential.** Every fixture decoded twice under
memcheck — once on `uninit` (the plane buffer is uninitialised, so any read of
it is a memcheck error) and once on `zeroed`. **128 fixtures × 2 arms = 256
runs executed, zero uninitialised-value errors, identical error-signature sets
on all 128.** 126 of the fixtures decode; two refuse. The sweep was run in two
pieces — 127 fixtures on the pre-rebase tree, then the fixture that arrived
with `d16af6c3` (`422_header_edge16_walk.obu`, 128 bytes) run separately under
both arms on `f01e9738`:

```
422_header_edge16_walk.obu    uninit 0    zeroed 0    CLEAN (identical)
raw, uninit arm: ERROR SUMMARY: 0 errors from 0 contexts
raw, zeroed arm: ERROR SUMMARY: 0 errors from 0 contexts
```

Those two runs are not vacuous, and that needed checking rather than assuming:
the fixture refuses, so "0 errors" could have meant "the decoder allocated
nothing". It did not. `EC_AV1_SUBSIZE_GUARD_TRACE=1` on it reports six
`note_subsize_guard` sites reached, with the refusal FIRING at
`decode.rs:37522` (`bsize=16x16 part=2 codable=false`) — that site is inside
the tile walk, which is downstream of the three `fresh_plane` calls at
`decode.rs:36007`/`36028`/`36040`. So the plane was allocated, partially
written, and abandoned mid-walk, and memcheck saw an `uninit` buffer with a
real partial write in it and still found nothing.
>
> One consequence for §2.3: this fixture prints **no** `SENTINEL_CENSUS` line,
> because the refusal short-circuits through `?` before the census at the tail
> of `decode_key_frame_tile_with_cdfs`. Its contribution is therefore 0 scanned
> and 0 holes, which is why the §2.3 totals are identical for 127 and for 128
> files — and also why the census's `scanned` pairing cannot be dropped: a
> fixture that contributes nothing looks exactly like one never looked at.
>

> **Non-vacuity, by mutation.** A temporary env-gated read of the plane's last
> slot (`v[n-1]`, added to `fresh_plane`, reverted before commit) makes the same
> binary on the same fixture report **12 errors from 4 contexts**:
> `Conditional jump or move depends on uninitialised value(s)` ×3,
> `Use of uninitialised value of size 8`. The instrument sees plane-sourced
> uninitialised reads when they exist.
>
> That run also shows the ambient-content hazard concretely: the luma plane
> (`n=30720`) errored and the two chroma planes (`n=7680`) happened to be handed
> already-zero memory and did not. A same-machine run is not proof; a
> *different* poison is.

**(b) Differential poisoning.** Decode each fixture under two DIFFERENT
impossible values for the plane buffer and compare the output hash. A read
whose value reaches the picture changes the hash when the poison changes.

| corpus | arms | result |
|---|---|---|
| 126 committed fixtures | `0xDEAD` / `0xBEEF` / `alloc_zeroed` | **126/126 byte-identical** |
| `440_request_is_422` | — | REFUSED (§2.3) |
| 4K 4:2:0, 4K 4:2:2, 1080p 4:2:0 | `0xDEAD`, `0xBEEF`, `0x0080`, `0x0001`, `0x0FFF`, `0x1000`, `0x2000`, `zeroed` | **8/8 identical per stream** |

The last row needs a note on method. `plane_poison()` *refuses* a poison at or
below `MAX_LEGAL_SAMPLE` (4095), because a legal poison makes the region census
vacuous. To run the experiment anyway I temporarily removed that guard: the
**legal-range** poisons (`0x0001`, `0x0080`, `0x0FFF`, `0x1000`, `0x2000`) then
produce the same hash as the impossible ones. That matters because an
out-of-range poison can be hidden by a clamp — every legal sample is
`(1 << bit_depth) - 1` at most — while a legal-range poison is not. **No read
survives clamping, masking or branch selection.** The guard is restored.

### 2.5 Verdict of the census

* **Region 1 (padded tail):** exists (4.68 M samples), is never written, and is
  **never read** — memcheck over 254 runs and 8 poisons agree.
* **Region 2 (true extent):** **0 holes** across 1.47 G samples on every current
  fixture; the historical 224-sample case is behind a refusal.
* **A third category, which neither instrument can see: `pix_write`.** Its
  `prev` argument is the destination sample's own value, read at four call
  sites on **every** reconstruction store. When a block is the first to write
  a sample, that read returns the buffer's initial content. It is still a read
  of a `u16` that (on the `uninit` arm) was never written.
  * Neither instrument reports it, and that is not a gap in them: `prev` is
    only consumed inside `if *PIXPROBE_ON`, so the value never reaches a
    branch, an address computation or a syscall — memcheck has nothing to
    complain about, and no hash can move. Verified directly: a **debug** build
    (no optimisation to dead-code-eliminate the load) under memcheck on the
    `uninit` arm still reports `0 errors from 0 contexts`.
  * So the honest reading is: the decoder provably does not *depend* on the
    buffer's initial content, and `pix_write` is the one place it reads it
    anyway. The shipped `Zeroed` closure deletes that read along with the rest,
    which is a second, independent reason to take it.
* **A constraint on the census arms that is not obvious from their names.**
  `fresh_plane` returns `vec![plane_poison(); n]` BEFORE it matches on
  `plane_alloc()`, so `PadZero` and `ExtZero` never run while
  `EC_AV1_PLANE_SENTINEL` is set — and that flag is exactly what both census
  functions require (each returns early when it is off). Those two arms are
  therefore meaningful only under **memcheck with the sentinel OFF**, where
  the zeroed/uninitialised split between the regions is what memcheck keys on;
  under the sentinel they are inert. The order is deliberate and stays:
  moving the sentinel check below the match would let the region census scan a
  buffer the sentinel never poisoned, which it cannot recover from.
* **Neither is reachable today.** That is a statement about today's corpus, not
  a proof: a hostile file chooses its own subsize values, and the refusal that
  closes region 2 lives in the tile walk, not at the allocation. Hence the
  decision below rather than "the premise holds".

---

## 3. The decision, and the evidence behind it

The charter's rule: *if (b) is within noise of (a) and close to the baseline,
implement (b) and prove determinism; if (b) is expensive and (a) is too, leave
the allocation alone.*

* (b) **is** within noise of (a) — 0.03 % apart at all three sizes.
* (b) is **not** within noise of the baseline at 4K: +0.84 % with a 6–10 ms
  run-to-run spread, i.e. ~6σ. So the charter's first branch is not literally
  satisfied.

I took (b) anyway, and the reason is the shape of the risk, not the size of the
number:

1. **The alternative is not "keep the unsafe", it is "keep asserting a premise
   that has already been withdrawn once."** That premise is what
   `lanes/unwritten-dep.report.md` §12 filed, and it produced a caller-visible
   defect. A 0.84 % regression is visible in a benchmark; a withdrawn SAFETY
   premise is invisible until it ships.
2. **The sign of the cost is not stable across resolutions** (§1). A decision to
   pay it should be made knowing it is ~0.85 % at 4K and a ~1.6 % *win* at 1080p,
   not "always 0.8 %".
3. **The decision is reversible in one line.** `EC_AV1_PLANE_ALLOC=uninit`
   restores the status quo exactly, which is only true because the closure is an
   env arm and not a build flag.

**Determinism proof** — new gate
`stream::tests::the_frame_does_not_depend_on_the_plane_buffers_initial_content`
(`stream.rs:48496`), which is exactly the charter's test:

* four child processes, same ten fixtures each: `EC_AV1_PLANE_SENTINEL=0xDEAD`,
  `EC_AV1_PLANE_SENTINEL=0xBEEF`, sentinel-off + `EC_AV1_PLANE_ALLOC=uninit`,
  sentinel-off + `EC_AV1_PLANE_ALLOC=zeroed`;
* one FNV-1a hash per fixture over every shown frame's Y/U/V samples; all four
  arms must agree, per fixture;
* **non-vacuity first, synthetic**: two pictures differing in ONE sample must
  hash differently, or every equality below would be vacuous;
* every arm must report `frames > 0`, so a child that decoded nothing cannot
  agree with another child that decoded nothing.

Fixture list deliberately small: a first revision used the 1920x792 film
fixtures and took **252 s** in a debug build, which is not a gate anyone keeps.
The committed ten (4:2:2 12-bit key / inter / sb128 intrabc / 17-frame odd
size, 4:4:4 lossless-sb64 / intrabc-rect4 / 128-root-mu, a padded odd size, an
off-tile chroma walk, superres+CDEF+LR) run the gate in **4.6 s**.

---

## 4. What changed

* `decode.rs` — `PlaneAlloc` (five arms), `plane_alloc()`, `zero_pad()`,
  `zero_extent()`, `plane_poison()` (a settable poison so two-value
  differential poisoning is possible; a poison ≤ 4095 is still refused),
  `MAX_LEGAL_SAMPLE`, the pad-tail census (`PAD_UNWRITTEN_SAMPLES`,
  `PAD_CENSUS_SCANNED` + accessors), `census_unwritten` extended to scan both
  regions, `census_unwritten_final` switched to `plane_poison()`,
  `fresh_plane` taking the plane geometry, and **`PlaneAlloc::Zeroed` as the
  default**, which puts the `unsafe` behind an env arm.
* `stream.rs` — the determinism gate, its child, and the synthetic control.
* `examples/alloc_timing.rs` — the decode-wall-time / page-fault harness.
* Hunks are confined to the allocation path: `decode.rs` `fresh_plane` and the
  six `PlaneBuf { … }` construction sites (3 planes × the key and inter frame
  functions), plus the census block next to them. The peer lane
  `lane/av1subsizesweep` touches `decode.rs` at ~37063 and ~57061 (a
  superblock arm and its tests); there is no overlap.

### SAFETY comment

Rewritten to state **what can be read before it is written** — the three named
regions with their measured counts, the memcheck result, the refusal that
covers region 2, and the fact that the shipped arm never reaches the `unsafe`
at all. The withdrawn premise ("written sample-for-sample before anything reads
it") is gone; nothing in the comment asserts a premise a reviewer cannot
re-run.

---

## 5. Gates

```
cargo check -p ec-av1 --tests                                     ok
cargo test -p ec-av1 --lib -- 422                    14 passed, 0 failed
cargo test -p ec-av1 --lib -- 444                    50 passed, 0 failed
cargo test -p ec-av1 --lib -- sub8                   13 passed, 0 failed
cargo test -p ec-av1 --lib -- lossless               34 passed, 0 failed
```
Re-run on the rebased tree (`f01e9738`), where main's own `1f4fcc84` guard for
the census child test is present:
```
cargo test -p ec-av1 --lib -- the_frame    13 passed, 0 failed
    (includes the_frame_does_not_depend_on_the_plane_buffers_initial_content)
cargo test -p ec-av1 --lib -- unwritten      2 passed, 0 failed
```

**One gate was red on the lane's original base and is not this lane's.** On
`2d8fa8a4`, `cargo test -p ec-av1 --lib -- unwritten` reported
`1 passed, 1 FAILED`: `stream::tests::uwdep_child_unwritten_census` opens with
an unconditional `assert!(EC_AV1_PLANE_SENTINEL is set)` while sitting in the
crate's default test list, so any standalone run of that filter fails. Main
commit `1f4fcc84` ("the census child test skips in a normal suite run instead
of reddening it") adds the entry guard and it is **not** an ancestor of this
lane's original base. Nothing in this lane touches that test.

---

## 7. Rebase onto `f01e9738`

Main moved while this lane was measuring. The branch is rebased onto `f01e9738`
and every headline number was re-taken there; the only conflict was in the
`census_unwritten` doc comment, resolved by keeping main's coverage paragraph
("this census runs at TWO of the four places `apply_deblock` is entered from")
*and* the two-region text, since both intents hold. Main's delta also adds
`chroma_plane_block_codable` to the 16-level edge arm — the guard that refuses
the 4:2:2 subsize by name.

What changed on the rebased tree, re-measured:
* the 3840x2160 4:2:0 row (§1) — `zeroed` +0.84 %, `fill` +0.92 %; same hashes,
  same `minflt`, same conclusion;
* the poison sweep — 126 identical, **2** refused: `440_request_is_422` and,
  newly, `422_header_edge16_walk`, which main's new guard refuses;
* the region census — **byte-identical totals**: 1,466,581,280 extent samples,
  0 holes; 22,787,296 pad samples, 4,676,128 holes; 38 fixtures.

---

## 8. Left undone, deliberately

* **A pooled plane buffer** would make the closure ~free: take the three plane
  buffers from a thread-local pool, zero each ONCE at creation, return them on
  drop. Every sample is then initialised by construction and the per-frame
  memset disappears. It is not done here because it changes ownership
  (`PlaneBuf` holds `Cow<'a, [u16]>` and is handed to the wavefront workers),
  which is a different lane's worth of risk than this charter took on.
* **The 1080p sign flip is unexplained.** §1.
* **Reader attribution for a region-2 read** is only as deep as "a reader that
  does not clamp". With no current fixture reaching region 2 there is nothing to
  attribute; `EC_AV1_PLANE_ALLOC=padzero` / `extzero` are in the tree for
  whoever gets one — under memcheck with the sentinel OFF, per the ordering
  constraint in §2.5.