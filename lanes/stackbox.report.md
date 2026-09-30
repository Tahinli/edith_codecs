# lane-stackbox — the by-value stack class in the sibling crates

**Branch:** `lane-stackbox` (worktree `/home/tahinli/.cache/wt/stackbox`, base `main` d202b8b4)
**Scope:** `ec-opus`, `ec-ac3`, `ec-vp9`. **`ec-av1` untouched** (another lane owns it).

## The class

A large struct returned **by value** from a public constructor is a stack cost,
not a heap cost, and the cost is paid by the **caller**. Constructors nest, so
one value's `size_of` is the *multiplier*, not the cost.

`crates/ec-av1/src/stack_budget.rs` (merged d202b8b4) fixed and gated this for
`ec-av1`. This lane sweeps the three sibling codecs. **Layout only** — no codec
behaviour, no defaults, no bitstream output changes.

## 1. The measured inventory

Every number is `std::mem::size_of`, measured with a throwaway probe test in
each crate, then re-measured permanently by each crate's new
`the_measured_inventory_is_accurate` gate (the probe was deleted; the gate is
the instrument). One command reproduces the whole table per crate:

```
export CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/stackbox EC_NOMEMGUARD=1
cd /home/tahinli/.cache/wt/stackbox
cargo test -p <ec-opus|ec-ac3|ec-vp9> --lib -- stack_budget -- --nocapture
```

### Headline: before → after

| crate | type | before | after | shrink | boxed fields |
|---|---|---|---|---|---|
| `ec-opus` | `Encoder` | **87,024** | **4,072** | 21.4x | 6× `Option<Box<SilkEncoder\|SilkStereoEncoder>>`, `silk_buf: Box<[u8; N]>` |
| `ec-opus` | `Decoder` | 7,576 | **1,280** | 5.9x | `silk: Box<SilkDecoder>` |
| `ec-ac3` | `Ac3Decoder` | **20,024** | **3,160** | 6.3x | `Core::{exps, bap, coeffs, delay}` |
| `ec-vp9` | `Decoder` | **10,944** | **768** | 14.3x | `frame_ctxs: Box<[FrameContext; 4]>`, `ctx: Box<FrameContext>` |

The sibling-lane pre-fix figures (87,024 / 20,024 / 10,944) **reproduced
exactly**. All four types are now under the 8,192 B budget, the worst at 49.7%.

**Correction to the charter's brief:** `silk_buf` is
`1+1+2*2+3*1275 = 3,831` bytes, not 4,088 as the contract stated. `Encoder`'s
pre-fix field breakdown: 59,232 (3× `SilkStereoEncoder` @ 19,744) + 19,944
(3× `SilkEncoder` @ 6,648) + 3,831 (`silk_buf`) + 2,168 (`TonalityAnalysis`)
+ 1,592 (`CeltEncoder`) + ~297 scalars/`Vec`s = 87,024.

### Full inventory (printed by the gates, post-fix)

`ec-opus` — `SilkStereoEncoder` 19744 (crate-private), `SilkEncoder` 6648
(crate-private), `SilkDecoder` 6304, **`Encoder` 4072**, `TonalityAnalysis`
2168 (crate-private), `CeltEncoder` 1592 (crate-private), **`Decoder` 1280**,
`CeltDecoder` 1168, `CeltFrameDiag` 424, `SilkDecIndices` 104,
`MultistreamDecoder` 96, `MultistreamEncoder` 88, `RangeEncoder` 80,
`AnalysisInfo` 60, `RangeDecoder` 56, `SilkFrameDiag` 56, `EncSnapshot` 48,
`Packet` 40, `Toc` 3, `Application` 1, `Mode` 1, `Bandwidth` 1.

`ec-ac3` — **`Ac3Decoder` 3160 (38.6%)**, `Core` 2864 (crate-private),
`Ac3Encoder` 2784 (34.0%), `Imdct` 472, `AudioFrame` 96, `Eac3Bsi` 96,
`EncodeStats` 72, `Bsi` 56, `Mantissas` 56, `Allocation` 56, `FrameInfo` 48,
`DeltaBa` 32, `SyncInfo` 24, `Channel` 12, `EncoderConfig` 12, `Options` 8,
`BitAllocParams` 5, and six 1-byte enums.

`ec-vp9` — `FrameContext` 2039 (crate-private), **`Decoder` 768 (9.4%)**,
`Picture` 96, `BoolDecoder` 40.

## 2. What dominated each type, and what was boxed

Rule applied: box the field(s) that **dominate**. A field was not boxed to
make a number look better.

**`ec-opus::Encoder`.** The six SILK slots are 79,176 B = **91%** of the
struct; `silk_buf` is a further 3,831. Boxed as six `Option<Box<..>>` plus
`Box<[u8; MAX_SILK_PACKET_BYTES]>`. `analysis` (2,168) and `celt` (1,592) were
**left inline** — 3,760 combined is already under budget, and a `Box` per
field buys no budget while costing a per-frame indirection on a hot path.
The boxing is a natural fit: only one of the six slots is ever alive at a time
(the mode picks one), so the heap pays for one encoder and the stack for six
pointers. The `Option` already existed for exactly that laziness.

**`ec-opus::Decoder`.** `silk: SilkDecoder` is 6,304 = **83%** of the struct.
Boxed. This type was *under* budget at 7,576 but at 92.5% of it, and `ec-av1`'s
own docs name "a bound the largest type sits just under is a bound the next
lane will quietly raise" as the failure mode.

**`ec-ac3::Ac3Decoder`.** 296 B of header plus `Core` at 19,728. Inside `Core`,
four inline arrays are 16,896 = **86%**: `coeffs` 7,168, `delay` 6,144,
`exps` 1,792, `bap` 1,792. All four boxed as `Box<[[T; COEFFS]; N]>`.
Indexing, `&mut self.f[ch]` and `bitalloc::compute(.., &self.exps[ch], &mut
self.bap[ch])` all read through the box — **no call site needed rewriting**.

**`ec-vp9::Decoder`.** `frame_ctxs: [FrameContext; 4]` is 8,156 = 75%. Boxing
it alone left **2,800 — under budget, but 2,039 of that (72.8%) was the single
inline `ctx` field**, i.e. one field carrying nearly all of the remainder.
`ctx` was boxed too, giving 768. The three whole-array assignment sites needed
`Box::new`; the three `frame_ctxs[..] = self.ctx.clone()` copy-out sites
became `.as_ref().clone()`.

## 3. API compatibility — checked BEFORE any edit

**No public field type changed in any of the three crates.** Every field boxed
is private or `pub(crate)`: `ec-opus::Encoder` and `::Decoder` fields are all
private; `ec-ac3::decode::Core` is `pub(crate)` and `Core` itself is
crate-private; `ec-vp9::Decoder` fields are all private.

Public method signatures are untouched — `Encoder::new`, `Decoder::new`,
`Ac3Decoder::new`, `Ac3Decoder::with_options`, `vp9::Decoder::new` keep exact
parameter and return types. A consumer that constructs an encoder and calls
`encode` compiles and runs unchanged. The only observable difference is a few
heap allocations at construction that were previously zero; these are not on
the per-frame path and are not part of any contract here.

`ec-vp9::Picture`'s `pub` fields were **not** touched — 96 B of `Vec` handles.

## 4. No output change — before/after, with counts

Baselines taken on the **unmodified** tree at d202b8b4, before any edit.

| crate | gate | before | after |
|---|---|---|---|
| `ec-opus` | `--lib` | 36 passed, 0 failed | **42** passed, 0 failed (+6 gate) |
| `ec-opus` | `--test conformance` | 28 passed, 0 failed, 16 ignored | **28 passed, 0 failed, 16 ignored** |
| `ec-ac3` | `--lib` | 36 passed, 0 failed | **43** passed, 0 failed (+7 gate) |
| `ec-ac3` | `--test decode_matrix` | 8 passed, 0 failed | **8 passed**, 0 failed |
| `ec-ac3` | `--test encode_matrix` | 6 passed, 0 failed | **6 passed**, 0 failed |
| `ec-vp9` | `--lib` | 10 passed, 0 failed | **16** passed, 0 failed (+6 gate) |
| `ec-vp9` | `keyframe_exact` | 3 passed | **3 passed** |
| `ec-vp9` | `inter_pixels_exact` | 3 passed | **3 passed** |
| `ec-vp9` | `hbd_exact` | 1 passed | **1 passed** |
| `ec-vp9` | `subsampling_exact` | 3 passed | **3 passed** |
| `ec-vp9` | `odd_dimensions_exact` | 2 passed | **2 passed** |
| `ec-vp9` | `scaledref_exact` | 3 passed | **3 passed** |
| `ec-vp9` | `intraonly_exact` | 1 passed | **1 passed** |

**Every bit-exactness count is identical before and after.** The only delta in
any line is the `+N` gate tests this lane added to `--lib`.

`ec-opus --test conformance` is the crate's own bit-exactness gate: it holds
the closed loop range-state-exact against this crate's RFC-vector-verified
decoder. 16 tests are `#[ignore]`d on unmodified `main` and stay ignored.

**A pre-existing red worth recording:** on unmodified `main` in a bare
worktree, `ec-ac3 encode_matrix::real_fixtures_round_trip…` and
`vp9 keyframe_exact::lossless_64_matches_ffmpeg` both FAIL with a
missing-fixture error (`No such file or directory`), not a decode mismatch.
The worktree ships no `fixtures/` payload; after copying it from the primary
checkout both are green. Noted so a reader does not mistake the pre-edit red
for a regression.

## 5. The gates

Three new `stack_budget.rs` modules, each `#![cfg(test)]`, each modelled on
`crates/ec-av1/src/stack_budget.rs` and each with `STACK_BUDGET = 8192`
(the same number, derived the same way: an unconfigured caller hands a crate at
least 2 MiB; deepest public by-value chain × worst public return × 8 must fit).

Per crate, seven tests:

1. `the_measured_inventory_is_accurate` — the `BY_VALUE_TYPES` table vs live
   `size_of`, keyed **by name** (positional zip would assert the wrong type's
   size when two types share one). Prints the sorted table.
2. `every_by_value_type_fits_the_stack_budget` — the bound asserted **by name**
   per row, plus the chain×worst×8 derivation re-asserted so neither side rots.
3. a **box pin** — a source scan over the real field declarations with a
   **negative control** that the unboxed spelling is absent (otherwise the
   scan could be matching a doc comment).
   - opus: `the_crate_private_silk_encoders_stay_behind_a_box`
   - ac3: `the_crate_private_core_stays_behind_a_box`
   - vp9: `the_frame_contexts_stay_behind_a_box`
4. **capability arm** — really constructs the heavy types on a deliberately
   tight thread, so the old layout **aborts the process** instead of costing
   invisible margin. Overridable via `EC_{OPUS,AC3,VP9}_TIGHT_STACK_BYTES`.
5. `every_by_value_public_return_is_in_the_inventory` — source scan
   re-deriving by-value `pub fn` returns from the crate, so a lane adding a
   40 KB struct to a `pub fn` reds until it is measured and listed.
6. `the_inventory_scan_is_not_vacuous` — the **synthetic control** on the
   matcher (5 positive + 8 thin spellings), sharing the matcher with the sweep.
7. `every_by_value_module_is_scanned` — the scanned file list is **derived**
   from `lib.rs`, so a new module reds.

Measured tight-stack thresholds (one process per point):

| crate | gate | deepest measured need | margin |
|---|---|---|---|
| `ec-opus` | 256 KiB | pre-fix layout *completed* at 544 KiB, overflowed at 528 KiB | current layout ok at 48 KiB, overflow at 32 KiB → 5.3x |
| `ec-ac3` | 1 MiB | ~80 KiB (ok 81,920 / overflow 77,824) | 12.8x |
| `ec-vp9` | 256 KiB | constructs `Decoder::new` fine | — |

The `ec-opus` gate is deliberately at **256 KiB, not 1 MiB**: the pre-fix
layout *passes* a 1 MiB gate, so a 1 MiB gate would have been a formality. At
256 KiB the pre-fix layout aborts the process and the current one has 5.3x
margin.

## 6. Mutation proofs — the gate bites

Each crate: unbox one field, show the gate red **by name**, restore
**byte-exact** (sha256 verified), re-run green.

**`ec-opus`** — unboxed `silk_buf` back to an inline array. **2 tests red, both
naming it:**

```
the_crate_private_silk_encoders_stay_behind_a_box ... FAILED
  encoder.rs::Encoder::silk_buf is no longer declared
  `silk_buf: Box<[u8; MAX_SILK_PACKET_BYTES]>`. An inline SILK encoder or
  scratch buffer there puts Encoder::silk_buf's bytes back on every caller's
  stack -- that is how Encoder was 87024 bytes ...
the_measured_inventory_is_accurate ... FAILED
  Encoder is 7896 bytes, the inventory records 4072
```

**`ec-ac3`** — unboxed `coeffs`. **3 tests red, each named** (inventory 10,320
vs 3,160; budget 1.26x over; box pin). The negative control was separately
proven non-vacuous: with `coeffs` inline but both box spellings injected **as
comments**, all positive pins pass and only the negative control fires — exactly
the comment-masking failure it exists for.

**`ec-vp9`** — I ran this one myself, after adding the `ctx` pin. Unboxed
`ctx`. **2 tests red, both naming it:**

```
the_frame_contexts_stay_behind_a_box ... FAILED
  decode.rs::the ctx field is no longer declared `ctx: Box<FrameContext>`.
  An inline `FrameContext` there puts 2039 bytes back into every caller's
  stack -- five of them were 10195 bytes, which is how `Decoder` was 10944
  bytes and 1.34x over the by-value budget before the boxes.
the_measured_inventory_is_accurate ... FAILED
  Decoder is 2800 bytes, the inventory records 768
```

All three restores verified by sha256 identical before and after, plus a green
re-run (`ec-opus` conformance re-run too: 28 passed, 0 failed).

## 7. Honest notes / what was NOT done

- **`ec-av1` not touched.** No read-modify-write; only read as the precedent.
- **`.cargo/config.toml`'s `RUST_MIN_STACK` not lowered.** The point of the
  lane is that a consumer who never sees that file is safe.
- **No codec behaviour, default, or bitstream output changed.** The diff is
  field types, `Box::new` at construction, `&mut f` → `&mut f[..]` at four
  `silk_buf` borrow sites, and three `.clone()` → `.as_ref().clone()` at vp9
  copy-out sites.
- **`ec-vp9`'s inventory is genuinely small** — only 3 public by-value types,
  because 11 of its 13 `pub fn` lines return a primitive, `Self` or `()` and
  every other module's API is `pub(crate)` behind `Decoder`. The gate's floor
  is set to the real count (3) rather than padded to `ec-av1`'s 10; the assert
  is an exact equality whose failure message states the real number. Padding it
  would have required inventing types, so it was not padded.
- **`ec-vp9`'s matcher unwraps `Result`/`Option` in a loop**, not once: its one
  by-value public return is `Result<Option<Picture>>`, which a single unwrap
  reads as the non-type `"Option"`. A synthetic control case pins the
  double-wrapped spelling.
- **Not done, and named:** the `ec-opus` sweep found `CeltEncoder` (1,592),
  `TonalityAnalysis` (2,168) and `CeltDecoder` (1,168) still inline. All are
  under budget, so boxing them is a future move, not this lane's.
- **Full-suite validation is the integrator's.** Only scoped per-crate
  `--lib`, the named integration exactness tests, and the new gate module were
  run here; concurrent lanes made a project-wide run unsafe mid-flight.

## 8. Files

```
 crates/ec-ac3/src/decode.rs              | 25 +++++++++++-----------  (boxing)
 crates/ec-ac3/src/lib.rs                 |  1 +                       (mod decl)
 crates/ec-opus/src/encoder.rs            | 61 +++++++++++++++++--------  (boxing)
 crates/ec-opus/src/lib.rs                | 11 +++--                   (boxing + mod decl)
 crates/ec-vp9/src/decode.rs              | 36 ++++++++++++++-----------  (boxing)
 crates/ec-vp9/src/lib.rs                 |  1 +                       (mod decl)
 crates/ec-ac3/src/stack_budget.rs        | new,  995 lines, 8 tests
 crates/ec-opus/src/stack_budget.rs       | new,  949 lines, 7 tests
 crates/ec-vp9/src/stack_budget.rs        | new,  725 lines, 7 tests
 lanes/stackbox.report.md                 | this file
```

No commit, no push, no merge, no rustfmt — per the lane's constraints.
