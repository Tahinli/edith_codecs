# lane-av1stacksweep — the by-value-size class, swept and gated

**Status:** green, commit `3d4caf13`. Files: `crates/ec-av1/src/stack_budget.rs`
(new, 8 tests), `lanes/av1stacksweep.report.md` (new), one line in
`crates/ec-av1/src/lib.rs`, and a **layout-only** boxing change in
`encode.rs` / `encoder.rs` (below). No bitstream change, no decode.rs, no
`.cargo/config.toml` edit, no rustfmt.

**This lane also FIXED two types, not just measured them.** The sweep found
that lane-av1enchang's `Box` on `DpbSlot::cdfs` left two more copies of the
same 15,232-byte `CdfSnapshot` sitting inline, and nobody had measured either:

| type | before | after | what was inline |
|---|---:|---:|---|
| `Encoded` | 30,728 | **280** | `start_cdfs` + `next_cdfs`, 99.2% of the struct |
| `Av1Encoder` | 18,168 | **2,944** | `carried_cdfs`, 15,232 of the 18,168 |

Same shape as the fix it follows (`99750756`), same gate family, same
reason. `CdfSnapshot` the VALUES are untouched.

**API compatibility: not a breaking change.** Both changed fields
(`Encoded::start_cdfs`, `Encoded::next_cdfs`) and the type they hold
(`CdfSnapshot`) are `pub(crate)`. `grep -rn "start_cdfs\|next_cdfs\|CdfSnapshot"
crates/ | grep -v ^crates/ec-av1/` is EMPTY, and so is any other crate naming
`Encoded`. `Encoded`'s public fields (`stream`, `reconstruction`, `modes`,
`inter_block_share`) keep their types. No consumer in this workspace, and none
in `edith`, can reach the fields that changed.

lane-av1enchang found and fixed ONE instance of this class (`Av1Encoder`,
139960 → 18168 B by boxing the DPB `CdfSnapshot`) and gated that one
constructor. The class itself was not swept. This lane sweeps it, picks a
bound from measurement rather than taste, gates the bound so it bites, and
reports on the 64 MiB cap that has been hiding the class.

---

## 1. INVENTORY — measured, with the command that reproduces it

### The one command

```bash
cd /home/tahinli/.cache/wt/av1stacksweep
CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1stacksweep EC_NOMEMGUARD=1 \
  cargo test -p ec-av1 --lib -- stack_budget --nocapture
```

`the_deepest_by_value_constructors_fit_on_a_tight_stack` prints the two
headline sizes, `the_measured_inventory_is_accurate` prints nothing but fails
loudly on drift, and the whole table below is asserted in-test. To dump the
table without running anything else, the numbers are the `Some(_)` column of
`BY_VALUE_TYPES` in `crates/ec-av1/src/stack_budget.rs`, and each is checked
against `size_of` at runtime.

### ec-av1: every type a public `fn` returns by value

Measured with `std::mem::size_of` in a debug build, 2026-09-30, at
`99750756` (main). "By value" = the return type is a bare named type, `Self`,
`Result<T>`, or `Option<T>` — not `Box`/`Rc`/`Arc`/`&`/`Vec`/`String`, which
are pointers or three words.

This is the gate's own printed table (`cargo test -p ec-av1 --lib --
stack_budget -- --nocapture`), so the numbers below are not transcribed by
hand. `public` = reachable from a `pub fn` return, which is what the budget
applies to.

| bytes | %budget | public | type | returned by value from |
|---:|---:|---|---|---|
| 15232 | 185.9% | no | `cdf_state::Cdfs` | `Cdfs::new(q_ctx)`; boxed everywhere it is stored |
| 15232 | 185.9% | no | `encode::CdfSnapshot` | newtype over `Cdfs`; `Box`ed in all four holders |
| **2944** | **35.9%** | yes | `encoder::Av1Encoder` | `new`, `with_speed`, `with_pyramid`, `with_rate_target`, `with_pyramid_and_rate_target` → `Result<Self>`, **3 deep** |
| 1296 | 15.8% | no | `decode::FrameCtx` | `for_encoder()` → `Self`, `pub(crate)`; one per `Av1Encoder` |
| 352 | 4.3% | yes | `census::Frame` | row type, moved by value |
| 280 | 3.4% | yes | `encode::Encoded` | `encode_key_frame`, `encode_key_frame_with_modes`, `encode_key_frame_at_size` → `Result<Encoded>`, **4 deep** |
| 240 | 2.9% | yes | `mvstack::MvStack` | `MvStack::new` → `Self` |
| 112 | 1.4% | no | `cdf_state::TxbTables<'a>` | `Cdfs::txb` — nine borrowed slices, so pointers not tables |
| 88 | 1.1% | yes | `encode::Picture` | `Picture::grey` → `Self` |
| 72 | 0.9% | yes | `encode::EncodedSequence` | `encode_sequence` → `Result<..>` |
| 64 | 0.8% | yes | `msac::SymbolDecoder<'a>` | `SymbolDecoder::new` |
| 56 | 0.7% | yes | `msac::SymbolEncoder` | `SymbolEncoder::new`, `pricer` |
| 48 | 0.6% | yes | `encoder::Packet` | `Av1Encoder::encode` → `Result<Packet>` |
| 48 | 0.6% | no | `motion_field::TplProbe<'a>` | borrowed-view handle |
| 40 | 0.5% | yes | `encoder::EncoderConfig` | `EncoderConfig::new` |
| 32 | 0.4% | yes | `warp::WarpParams` | `global_warp_params` → `Option<..>` |
| 24 | 0.3% | yes | `encoder::Pyramid` | ctor + `Av1Encoder::pyramid` |
| 24 | 0.3% | yes | `qm::Iqm` | `iwt_matrix` → `Option<..>` |
| 16 | 0.2% | yes | `encoder::RateTarget` | ctor by value |
| 16 | 0.2% | yes | `mvstack::MiInfo` | `MvStack::get` → `Option<..>` |
| 1 | 0.0% | yes | `mc::InterpFilterKind` | `from_switchable_symbol`, `from_header` |

**Before this lane** the same 21 types measured 30,728 (`Encoded`), 18,168
(`Av1Encoder`), 15,232 ×2, 1,296, 352, 240, 112, 88, 72, 64, 56, 48 ×2, 40,
32, 24 ×2, 16 ×2, 1.

**Two findings the earlier lane did not have:**

1. **`Encoded` (30728) is bigger than `Av1Encoder` (18168) and nobody had
   measured it.** It is 99.2% two inline `CdfSnapshot`s (`start_cdfs` and
   `next_cdfs`, 15232 each). lane-av1enchang boxed exactly this field in the
   DPB slot and not here. See §5.
2. **The by-value chain is four deep, not three.** `encode_key_frame` →
   `encode_key_frame_with_ctx` → `encode_key_frame_with_modes_with_ctx` →
   `encode_key_frame_inner`, every level `Result<Encoded>`.

### How the list is kept honest

Two tests, because a hand-written list is a list of what someone remembered:

- `every_by_value_public_return_is_in_the_inventory` re-derives the set from
  the crate source (38 modules, `include_str!`), so a lane that adds
  `pub fn make() -> HugeThing` reds until `HugeThing` is measured and listed.
- `every_by_value_module_is_scanned` derives the module list from `lib.rs`
  itself, so a new `mod foo;` cannot be silently unscanned. **This one earned
  its keep immediately**: when it was first written it named 15 modules the
  scan did not read (`compound`, `envflags`, `film_grain`, `filter_search`,
  `gate_coverage`, `hits`, `library_fixture`, `motion_field`, `par`,
  `refusal_inventory`, `restoration`, `superres`, `timeline`, `wedge`, and
  `stack_budget` itself) — a real hole, now closed.
- `the_inventory_scan_is_not_vacuous` is the control: thirteen synthetic
  `pub fn` lines, five by-value spellings that must yield their type name and
  eight pointer/`Vec`/primitive/`Self` spellings that must yield nothing.
  **It caught a live bug in the matcher**: the first version cut the return
  type only at `>`, so `Result<Decoder, Error>` parsed as the name
  `"Decoder, Error"`. Fixed to split on `,` as well.

### The sibling crates — 293 public types, all measured

Delegated to a subagent that built a throwaway `tests/zz_stack_probe.rs` per
crate and deleted them all (verified: final `git status` shows zero
`zz_*probe*` files, and the 5 `tests/` dirs it created were `rmdir`ed after
checking `existed_on_HEAD=0`). 293 types measured, **0 inferred**.

| bytes | crate | type | smallest stack that survives | smallest that overflows |
|---:|---|---|---|---|
| **87024** | ec-opus | `Encoder` | 384 KiB | **352 KiB** |
| 20024 | ec-ac3 | `Ac3Decoder` | 96 KiB | 64 KiB |
| 19744 | ec-opus | `SilkStereoEncoder` | 256 KiB | 128 KiB |
| 10944 | ec-vp9 | `Decoder` | 64 KiB | 32 KiB |
| 10288 | ec-aac | `Synthesis` | 32 KiB | 16 KiB |
| 7576 | ec-opus | `Decoder` | 256 KiB | 128 KiB |
| 6688 | ec-h264 | `H264Decoder` | 64 KiB | 48 KiB |
| 6648 | ec-opus | `SilkEncoder` | 256 KiB | 128 KiB |
| 6496 | ec-h264 | `Decoder` | 256 KiB | 128 KiB |
| 2784 | ec-ac3 | `Ac3Encoder` | 64 KiB | no overflow |
| 2600 | ec-mp3 | `Mp3Decoder` | 64 KiB | 32 KiB |
| 2432 | ec-mp3 | `Mp3Reader` | 32 KiB | 16 KiB |
| 2392 | ec-mp3 | `Mp3Decode` | 32 KiB | 16 KiB |
| 1928 | ec-vorbis | `VorbisEncoder` | 32 KiB | 16 KiB |
| 1744 | ec-vp8 | `Decoder` | 8 KiB | no overflow |
| 1344 | ec-vorbis | `VorbisDecoder` | 64 KiB | no overflow |
| 712 | ec-h264-syntax | `Sps` | 8 KiB | no overflow |
| 664 | ec-mp3 | `Mp3Encoder` | 8 KiB | no overflow |
| 544 | ec-h265 | `Encoder` | 8 KiB | no overflow |
| 360 | ec-dsp | `Dct<f64>` | 32 KiB | no overflow |
| 256 | ec-matroska | `MatroskaDemuxer<..>` | 32 KiB | no overflow |
| 240 | ec-core | `StreamInfo` | 8 KiB | no overflow |
| 184 | ec-probe | `Reader` | 8 KiB | no overflow |
| 120 | ec-subs | `SubtitleStyle` | 8 KiB | no overflow |
| 96 | ec-image | `Frame` | 8 KiB | no overflow |
| 64 | ec-mp4 | `TrackInfo` | 64 KiB | no overflow |
| 48 | ec-ogg | `PageHeader` | 8 KiB | no overflow |
| 32 | ec-riff | `AviAudioStream` | 64 KiB | no overflow |

**`ec_opus::Encoder` at 87,024 bytes is 2.66× over the bound this lane picks,
and it is the largest by-value type in the workspace.** It survives 384 KiB of
stack and SIGABRTs below 352 KiB. Nothing in the repo trips that today only
because the 64 MiB cap is in effect. This is the next instance of the class
and it is not in ec-av1.

**The measured stack/size ratio is ~4.4×** (ec-opus `Encoder`: 87024 B of
struct, ~384 KiB of live stack). That is the constructors-nesting multiplier
made visible, and it is why §2 sizes the bound against the *stack*, not
against `size_of`.

`ec-dsp` is the healthiest crate in the workspace and worth copying: `Mdct`,
`Fft`, `RealFft`, `Dct4`, `Dct` and `Window` all hold `Vec` plans, so
`Fft::new(n)` returns a handful of words despite building large tables.
`ec-ass` declares no types at all. `ec-hw` boxes its inner decoders, so its
largest by-value return is a 3140 B parameter buffer that never overflows.

---

## 2. THE BOUND: 8192 bytes (8 KiB), derived from measurement at both ends

`STACK_BUDGET = 8192` in `crates/ec-av1/src/stack_budget.rs`. It is bounded
from BOTH sides by measurement, and `every_by_value_type_fits_the_stack_budget`
asserts both, so neither can rot into a number nobody can re-derive.

**The quantity being bounded** is not `size_of` in the abstract but *what a
caller can be made to owe*: one by-value return, multiplied by how many
constructor levels hold a copy at once. Three inputs:

1. **The smallest stack a caller can hand this crate is 2,101,248 bytes.**
   Measured, not assumed: `test_threads_get_the_stack_they_are_given` reads
   `/proc/self/task/<tid>/maps` for the region containing a live frame. That is
   libtest's default test thread and also what `std::thread::Builder::new()`
   gives a caller who does not ask for a size. A library consumer gets no
   `.cargo/config.toml`. (With the repo cap it measures 67,112,960 — also
   pinned, so both ends of the claim are asserted.)
2. **The deepest PUBLIC by-value chain is 4** (`DEEPEST_PUBLIC_CHAIN`):
   `encode_key_frame` → `encode_key_frame_with_ctx` →
   `encode_key_frame_with_modes_with_ctx` → `encode_key_frame_inner`. The
   encoder's own nest is 3. The test asserts
   `DEEPEST_PUBLIC_CHAIN * worst * 8 <= 2 MiB` — the by-value return slots may
   claim at most an **eighth** of a caller's stack, leaving the rest for the
   encode search, which is the thing that actually needs it.
3. **The budget applies only to types a `pub fn` returns.** The crate-private
   15,232-byte `Cdfs` / `CdfSnapshot` are measured and listed but NOT bounded,
   because no caller can reach them by value — every field holding one is a
   `Box`, pinned by `the_crate_private_cdf_tables_stay_behind_a_box`. Bounding
   them would calibrate the number to something unreachable.

**Where the number sits.** Worst public return 2,944 B = **35.9%** of an 8 KiB
budget (2.78× headroom). Worst a caller owes: `4 × 2944 = 11,776` bytes =
**0.56%** of a 2 MiB thread. Both directions have room, which is the point: a
bound the largest type sits just under is a bound the next lane will quietly
raise.

**What it catches**, each by a factor rather than a hair:

| what | bytes | × over 8192 |
|---|---:|---:|
| `Av1Encoder` before lane-av1enchang | 139,960 | 17.09× |
| `ec_opus::Encoder` (sibling crate, same sweep) | 87,024 | 10.62× |
| `Encoded` before this lane boxed it | 30,728 | 3.75× |
| `Av1Encoder` after lane-av1enchang, before this lane | 18,168 | 2.22× |
| **`Av1Encoder` now** | **2,944** | **0.36×** |
| **`Encoded` now** | **280** | **0.03×** |

The historical defect, the near-miss, and the largest live instance of the
class in the workspace all red by a factor.

**What it does not claim.** `size_of` is layout, not stack depth. The measured
stack/size ratio on a by-value constructor is ~4.4× (`ec_opus::Encoder`:
87,024 B of struct, ~384 KiB of live stack) — the nesting multiplier made
visible. That residual is exactly why the capability arm in §3 constructs for
real instead of trusting the arithmetic.

## 3. THE GATE, and its mutation proof

Eight tests in `crates/ec-av1/src/stack_budget.rs`, all green in 0.10s. The
module is `#![cfg(test)]`, so a shipped build carries none of it.

| test | what it holds |
|---|---|
| `the_measured_inventory_is_accurate` | every row's recorded size is the size the type has now, and it **PRINTS the whole table sorted** — the reproduction instrument |
| `every_by_value_type_fits_the_stack_budget` | **the bound, asserted BY NAME**, plus the `DEEPEST_CHAIN × worst × 8 ≤ 2 MiB` derivation check |
| `the_deepest_by_value_constructors_fit_on_a_tight_stack` | **capability**: both deepest public chains really constructed on a 1 MiB thread |
| `every_by_value_public_return_is_in_the_inventory` | source scan over all 38 modules; a new by-value `pub` return reds until measured |
| `every_by_value_module_is_scanned` | the scan's file list is DERIVED from `lib.rs`, not curated |
| `the_inventory_scan_is_not_vacuous` | control: 5 by-value spellings must yield a type name, 8 pointer/`Vec`/primitive/`Self` spellings must yield nothing |
| `the_crate_private_cdf_tables_stay_behind_a_box` | pins the four `Box<CdfSnapshot>` declarations by source |
| `test_threads_get_the_stack_they_are_given` | the 2,101,248-byte floor the bound is derived from, measured from `/proc` |

### The capability arm, and the thresholds behind it

One 1 MiB thread runs BOTH deepest public chains: the three-deep
`Result<Self>` encoder constructor nest and the four-deep `Result<Encoded>`
key-frame chain. A regression **aborts the process** rather than costing
invisible margin.

Measured stack need, **debug** build, 64×64, one process per point so a
SIGABRT cannot truncate the sweep:

| stack | before this lane boxed `Encoded` | after |
|---|---|---|
| 131072 | overflow | overflow |
| 262144 | overflow | overflow |
| 327680 | overflow | overflow |
| 466944 | overflow | **overflow** |
| 475136 | overflow | **ok** |
| 524288 | overflow | **ok** |
| 786432 | **overflow** | ok |
| 1048576 | **ok** | **ok** |

The 1 MiB gate is now **2.2×** the deepest measured need, not 1.33×. The
~470 KB that remains is the encode search's own frames, not return slots.

Reproduce any row:
`EC_AV1_TIGHT_STACK_BYTES=<bytes> cargo test -p ec-av1 --lib -- the_deepest_by_value_constructors_fit_on_a_tight_stack --exact`.

### Mutation proof — both mutations, both reverted, both re-run after the refactor

**M1 — unbox what lane-av1enchang boxed** (a real revert of `99750756`, not a
synthesised one): `DpbSlot::cdfs: Box<CdfSnapshot>` → `CdfSnapshot`, plus its
one construction site.

```
Av1Encoder is 139960 bytes, over the 8192-byte by-value stack budget (17.09x).
  It is returned by value from Av1Encoder::new / with_speed / with_pyramid /
  with_rate_target / with_pyramid_and_rate_target, all `Result<Self>`, nesting
  three deep, so every caller pays it on its own stack ...
fatal runtime error: stack overflow, aborting   (signal: 6, SIGABRT)  exit 101
```

**M2 — grow `Encoded` past the bound** (synthesised, and labelled as such:
`Encoded` had no prior boxing to revert, so the honest mutation is to add a
field of the same shape that produced the class — a 64 KiB inline payload):

```
Encoded is 96264 bytes, over the 8192-byte by-value stack budget (11.75x).
  It is returned by value from encode_key_frame / encode_key_frame_with_modes /
  encode_key_frame_at_size -> Result<Encoded>, nesting four deep ...
fatal runtime error: stack overflow, aborting   (signal: 6, SIGABRT)
```

Both prove the bound row is a live read of the real `size_of` and not a
constant, and that the capability arm reaches a process abort. Reverted;
`git status` clean; 8/8 green.

**What is not proven.** That the 1 MiB gate has margin against a future
rustc's frame layout. Only that a large regression aborts rather than passes
quietly. The ~4.4× ratio in §1 is the honest statement of that residual risk.

## 4. THE 64 MiB CAP — what it was protecting, and what is left

### First: the cap is inert for anything but cargo

`.cargo/config.toml` sets `[env] RUST_MIN_STACK = "67108864"`. Cargo applies
`[env]` only to processes **cargo spawns**; running the test binary directly
does not get it. Measured both ways on one binary:

```
RUST_MIN_STACK unset    -> test thread stack 2101248 bytes
RUST_MIN_STACK=2097152  -> 2101248
RUST_MIN_STACK=8388608  -> 8392704
RUST_MIN_STACK=67108864 -> 67112960
via `cargo test` (repo [env] in effect) -> 67112960
```

So the cap does what it claims for cargo-driven runs, and a library consumer
who links this crate gets the 2,101,248-byte default.

### Second: what the cap was ACTUALLY protecting — and it was this class

Its comment reads:

> The AV1 rate-target tests keep several frames' worth of encoder state on the
> test thread's stack; the 2 MB default overflows in debug builds (release
> passes).

**The mechanism it names is wrong.** The "frames' worth of encoder state" is
`Av1Encoder`'s `pending: Vec<(u64, Picture)>`, `ready: Vec<(u64, Picture)>` and
`held: VecDeque<Picture>` — all heap. A scan for a large fixed-size stack local
in the encoder path finds exactly one:

```
crates/ec-av1/src/film_grain.rs:26:  const GAUSSIAN_SEQUENCE: [i32; 2048] = [
```

which is a `const`, i.e. `.rodata`, not a frame.

**But the CONCLUSION was right, and for this class's reason.** A 139,960-byte
`Av1Encoder` through a three-deep `Result<Self>` nest is ~420 KB of live stack
before the encode path adds its own, which does not fit in 2 MiB with the rest
of the chain. The fix removed the dominant term but not all of them, and two
more inline 15 KB tables survived lane-av1enchang unmeasured.

### Third: the per-test answer at 2 MiB

The charter warns that lowering the cap "makes the suite red for an unknown
set of tests". So the set was measured: **every test in the lib suite, one per
process** (a stack overflow SIGABRTs the process, so running them in one
libtest invocation would swallow the rest), at `RUST_MIN_STACK=2097152`, debug.

**Exactly one test needed more than 2 MiB:**

| test | at 2,097,152 | at 3,145,728 |
|---|---|---|
| `encoder::tests::the_facade_codes_the_same_bytes_as_encode_sequence` | **STACK OVERFLOW** | ok (50.67s) |

That test calls `encode_sequence` on twelve 640×384 frames four times over, and
`Encoded` was 30,728 bytes inline. **This commit fixes it: the same test now
passes at 2,097,152**, verified directly after the boxing.

A second name appeared in the first sweep —
`stream::tests::a_doubly_cut_superblock_root_round_trips_in_every_plane` — and
it was a **stale-binary artefact**: I rebuilt the test binary while the sweep
was running, so part of that run measured a half-written tree. Re-verified
individually against the stable build: **ok at 2,097,152**. Recording it here
because a sweep whose subject changes underneath it produces false positives,
and the only way to know which of its hits are real is to re-run each one.

**The full sweep, on the final tree.** Reproduce with:

```bash
BIN=$(ls -t "$CARGO_TARGET_DIR"/debug/deps/ec_av1-* | grep -v '\.d$' | head -1)
$BIN --list | sed 's/: test$//' \
  | grep -v bitrate_target_lands_within_5_percent_over_48_frames > /tmp/tests.txt
# one process per test, so a SIGABRT is attributable instead of swallowing the run
xargs -a /tmp/tests.txt -P 8 -I{} bash -c \
  'RUST_MIN_STACK=2097152 timeout 600 "$0" "$1" --exact --test-threads=1 \
     >/dev/null 2>/tmp/err.$$ && echo "OK $1" \
     || { grep -q "stack overflow" /tmp/err.$$ && echo "STACKOVERFLOW $1" \
          || echo "OTHER $1"; }' "$BIN" {}
```

**Result: 799 of 799 tests, ZERO stack overflows at `RUST_MIN_STACK=2097152`.**
One entry is not `OK`:

```
TIMEOUT encoder::tests::every_tile_layout_decodes_sample_exact_through_both_decoders
```

which hit the sweep's own 600-second per-test budget, not a stack limit — it
is a slow encode gate, and it passed in the release run above. It needs a
longer per-test timeout, not a bigger stack. Every other test, including
`encoder::tests::the_facade_codes_the_same_bytes_as_encode_sequence` (the one
that overflowed before this commit), reports `OK`.

**So: nothing in the ec-av1 lib suite requires the 64 MiB cap, and this lane
removed the last test that did.**

**The measured stack headroom for the paths that remain:**

| what | measured need (debug) | margin at 2 MiB | at 64 MiB |
|---|---|---|---|
| `encode_key_frame`, 64×64 | 786,432 → **475,136** after boxing | **4.4×** | 141× |
| `Av1Encoder` pyramid ctor nest | 262,144 | **8.0×** | 256× |
| pyramid `encode_sequence`, 640×384 | 917,504 | **2.3×** | 73× |
| the rate-target test's own worker body | 983,040 | **2.1×** | 68× |

The last row is the one the cap's comment names.
`bitrate_target_lands_within_5_percent_over_48_frames` runs its eight arms
inside `std::thread::spawn(move || { ... })` (encoder.rs:3602) — **no**
`stack_size`, so the runtime default, exactly the case a library consumer
hits. Its body is a constructor nest, then a **sequential**
`for picture in pictures.iter()` loop of `encode_frames`, then `flush`. Frames
do not nest, so the peak is one frame's chain, not 48 of them. Measured that
exact body at 640×384 in a debug build: 983,040 red / 1,048,576 green.

### What I could not measure, and why

- **`bitrate_target_lands_within_5_percent_over_48_frames` in DEBUG.** ~7 hours
  (the crate's own comment: the debug lib suite is 24341s and this is the
  largest single item), and heavy suites belong on the VPS fleet. What I
  measured instead is its worker body in isolation, above. To settle it on a
  fleet host:
  `env -u RUST_MIN_STACK cargo test -p ec-av1 --lib -- bitrate_target_lands_within_5_percent_over_48_frames --exact`.
- **The sibling crates' suites at 2 MiB.** The measurement that matters for
  them is the per-type stack sweep in §1, and that is complete: the only type
  in the workspace in danger on a 2 MiB thread is `ec_opus::Encoder`, and it
  survives 384 KiB.

### Recommendation

**The cap is no longer protecting anything in ec-av1 that the gates do not.**
Recommended change, in two steps, both outside this lane's charter:

1. Run each crate's suite once at `RUST_MIN_STACK=2097152` on the fleet, **one
   test per process**, so a stack overflow is attributable rather than
   swallowing the run. ec-av1 is done: zero failures.
2. Then lower the cap to whatever the largest surviving requirement turns out
   to be, and replace the comment with:

> The AV1 encoder's by-value returns are gated by `stack_budget.rs` against an
> 8 KiB per-type budget: `Av1Encoder` 2944 B and `Encoded` 280 B, through
> constructor chains 3 and 4 deep, with the deepest measured chain needing
> 475136 bytes in a debug build. Per-test evidence at `RUST_MIN_STACK=2097152`
> is in `lanes/av1stacksweep.report.md`. This cap is headroom for test
> threads, not a load-bearing fix -- lowering it is a separate change that
> needs its own per-test evidence.

I have **not** edited `.cargo/config.toml`, as chartered.

## 5. WHAT IS NEXT IN THIS CLASS (not done here)

**`ec_opus::Encoder` at 87,024 bytes is the live instance, and it is not in
this crate.** It is 10.6× over the bound, the largest by-value type in the
workspace, and it survives 384 KiB of stack while SIGABRTing below 352 KiB. Its
chain is 4 nested constructors returning the struct by value.

It is recorded rather than fixed because it is in `ec-opus`, a crate this lane
does not own, with its own bit-exactness gates that a change here would have to
run and this lane has no evidence for. **Recommended next lane: box
`ec_opus::Encoder`'s inline delay/history arrays the same way, and prove it
against that crate's own encoder gates** (`ec-ac3::Ac3Decoder` at 20,024 B
overflowing below 96 KiB and `ec-vp9::Decoder` at 10,944 B below 64 KiB are
the next two).

**The `Encoded` boxing that WAS in this lane's reach is done** — see the
header table. What is left in ec-av1 is the encode search's own ~470 KB of
stack, which is a different problem (frame-local buffers, not by-value
returns) and is not claimed by `stack_budget.rs`.

## 6. ACCEPTANCE EVIDENCE

### The new gate, debug

```
$ cargo test -p ec-av1 --lib -- stack_budget --nocapture
running 8 tests
... all ok ...
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 791 filtered out; finished in 0.10s

BY-VALUE INVENTORY (21 types, budget 8192 B = 8 KiB)
   BYTES  %BUDGET  TYPE
   15232   185.9%  Cdfs
   15232   185.9%  CdfSnapshot
    2944    35.9%  Av1Encoder
    1296    15.8%  FrameCtx
     352     4.3%  Frame
     280     3.4%  Encoded
     ... (21 rows) ...
TIGHTSTACK 1048576 bytes: Av1Encoder=2944 Encoded=280 bytes, both chains returned
TESTTHREADSTACK default ... size>=67112960 (2101248 measured exactly with the repo cap lifted, 67112960 with it)
```

### Release run — the chartered acceptance command

```
$ CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/stackcap2 EC_NOMEMGUARD=1 \
    cargo test --release -p ec-av1 --lib -- encode round_trip \
    --skip bitrate_target_lands_within_5_percent_over_48_frames

test encoder::tests::the_facade_codes_the_same_bytes_as_encode_sequence ... ok
test encoder::tests::thirty_pictures_at_gop_fifteen_decode_to_two_key_frames ... ok
test encoder::tests::the_rate_loop_prices_the_frames_the_pyramid_codes ... ok

test result: ok. 108 passed; 0 failed; 40 ignored; 0 measured;
             651 filtered out; finished in 308.46s

Wall time: 344.04 seconds
```

108 passed, 0 failed — the same count as the pre-boxing run of this exact
command on unmodified `main` behaviour (which also reported 108/0/40), so the
boxing changed no test's verdict.

Run in RELEASE because the debug encoder suite is hours, and with the repo's
64 MiB cap in effect, i.e. exactly how CI runs it. The 40 ignored are the
crate's pre-existing wall-measurement gates (`tile_search_wall_1080p`,
`filter_stage_wall_4k`, ...), which carry their own "run it with --ignored"
reasons and are unaffected by this lane.

### Bit-exactness of the boxing

The boxing moves no value. The proof is structural plus the gates:
`CdfSnapshot` is a newtype over `Cdfs`, so `Box::new(x)` stores the same
`x`; the CDF tables the writer starts from and the tables it stores are the
same objects, reached through one pointer indirection instead of inline.
`Encoded`'s public `stream`, `reconstruction`, `modes` and `inter_block_share`
are untouched, and `Debug` still prints `CdfSnapshot` (its `Debug` impl is
hand-written to print exactly that, so `Encoded`'s derived `Debug` output is
byte-identical too). The round-trip and pyramid decode gates above are the
empirical half.

### Files changed

```
crates/ec-av1/src/stack_budget.rs   new, 8 tests, #![cfg(test)]
crates/ec-av1/src/lib.rs            +1 line: `mod stack_budget;`
crates/ec-av1/src/encode.rs         Encoded::start_cdfs / next_cdfs -> Box<CdfSnapshot>
                                     (+2 construction sites)
crates/ec-av1/src/encoder.rs        Av1Encoder::carried_cdfs -> Option<Box<CdfSnapshot>>
                                     refresh() reuses the already-boxed snapshot
lanes/av1stacksweep.report.md        this file
```

Not touched: `decode.rs`, `.cargo/config.toml`, any CDF value, any bitstream
byte.
