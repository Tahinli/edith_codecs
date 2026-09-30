# lane-av1stacksweep — the by-value-size class, swept and gated

**Status:** green. `crates/ec-av1/src/stack_budget.rs` (new), one line in
`crates/ec-av1/src/lib.rs`. No codec behaviour touched: no bitstream change, no
decoder change, no `.cargo/config.toml` edit, no rustfmt.

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

| bytes | type | returned by value from |
|---:|---|---|
| **30728** | `encode::Encoded` | `encode_key_frame`, `encode_key_frame_with_modes`, `encode_key_frame_at_size` → `Result<Encoded>`, **4 frames deep** |
| **18168** | `encoder::Av1Encoder` | `new`, `with_speed`, `with_pyramid`, `with_rate_target`, `with_pyramid_and_rate_target` → `Result<Self>`, **3 frames deep** |
| **15232** | `cdf_state::Cdfs` | crate-private; the payload of every `CdfSnapshot` |
| **15232** | `encode::CdfSnapshot` | crate-private newtype over `Cdfs`; **inline twice in `Encoded`**, boxed in the DPB slot since lane-av1enchang |
| 1296 | `decode::FrameCtx` | crate-private; `for_encoder()` by value, one inline per `Av1Encoder` |
| 352 | `census::Frame` | row type, moved by value |
| 240 | `mvstack::MvStack` | `MvStack::new` → `Self` |
| 112 | `cdf_state::TxbTables<'a>` | `Cdfs::txb` — nine borrowed slices, so the move copies pointers |
| 88 | `encode::Picture` | `Picture::grey` → `Self` |
| 72 | `encode::EncodedSequence` | `encode_sequence` → `Result<..>` |
| 64 | `msac::SymbolDecoder<'a>` | `SymbolDecoder::new` |
| 56 | `msac::SymbolEncoder` | `SymbolEncoder::new`, `pricer` |
| 48 | `encoder::Packet` | `Av1Encoder::encode` → `Result<Packet>` |
| 48 | `motion_field::TplProbe<'a>` | crate-private, borrowed-view handle |
| 40 | `encoder::EncoderConfig` | `EncoderConfig::new` |
| 32 | `warp::WarpParams` | `global_warp_params` → `Option<..>` |
| 24 | `encoder::Pyramid` | ctor + `Av1Encoder::pyramid` |
| 24 | `qm::Iqm` | `iwt_matrix` → `Option<..>` |
| 16 | `encoder::RateTarget` | ctor by value |
| 16 | `mvstack::MiInfo` | `MvStack::get` → `Option<..>` |
| 1 | `mc::InterpFilterKind` | `from_switchable_symbol`, `from_header` |

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

## 2. THE BOUND: 32768 bytes (32 KiB), and why

`STACK_BUDGET = 32768` in `crates/ec-av1/src/stack_budget.rs`.

Four measured inputs, none of them taste:

1. **The smallest stack a caller can hand this crate is 2,101,248 bytes.**
   Measured, not assumed, by `test_threads_get_the_stack_they_are_given`
   reading `/proc/self/task/<tid>/maps` for the region containing a live
   frame. That is libtest's default test thread and also what
   `std::thread::Builder::new()` gives a caller who does not ask for a size.
   A library consumer gets no `.cargo/config.toml`. The same test measures
   67,112,960 bytes when the repo's cap is applied, so the two are pinned.

2. **The deepest public by-value chain is four frames**
   (`encode_key_frame` → `encode_key_frame_with_ctx` →
   `encode_key_frame_with_modes_with_ctx` → `encode_key_frame_inner`).
   Worst live stack from return slots alone is `4 × size_of::<T>()`.

3. **At 32 KiB that is 128 KiB — 6.1% of a 2 MiB thread.** A type at the
   budget still leaves 94% of the caller's stack for its own work.

4. **The two real thresholds bracket it.** A pre-fix `Av1Encoder` (139,960 B)
   is **4.27× over**; `Encoded` today (30,728 B) is **93.8% of** it. So the
   historical defect reds by a factor and today's worst type has a hair of
   headroom — which is the point, because it is the row that wants boxing next
   (§5).

`Encoded` at 93.8% of budget is deliberately not a violation. A budget the
current tree already breaks is not a budget, it is a to-do list, and the fix
belongs in the lane that does the boxing. The gate names it instead of
pretending it is comfortable.

**Cross-check against the workspace:** 32 KiB would also have caught
`ec_opus::Encoder` (87024, 2.66× over), `ec_ac3::Ac3Decoder` (20024, under —
would pass), `ec_opus::SilkStereoEncoder` (19744, under — would pass), and
every other sibling. So the bound is calibrated to catch the real defect
rather than to fit the incumbent.

**What it does not claim:** `size_of` is layout, not stack depth. The 4.4×
ratio above is why a type at the budget can still be uncomfortable in a deep
chain — which is exactly what the capability arm in §3 is for.

---

## 3. THE GATE, and its mutation proof

Seven tests in `crates/ec-av1/src/stack_budget.rs`, all green in 0.05s.

| test | what it holds |
|---|---|
| `the_measured_inventory_is_accurate` | every row's recorded size is the size the type has now |
| `every_by_value_type_fits_the_stack_budget` | **the bound, asserted BY NAME** |
| `the_deepest_by_value_constructors_fit_on_a_tight_stack` | **capability**: really constructs both heavy chains on a 1 MiB thread |
| `every_by_value_public_return_is_in_the_inventory` | source scan; a new by-value return reds until measured |
| `every_by_value_module_is_scanned` | the scan's file list is derived from `lib.rs`, not curated |
| `the_inventory_scan_is_not_vacuous` | control: 5 by-value spellings yield a name, 8 thin ones yield nothing |
| `test_threads_get_the_stack_they_are_given` | the 2 MiB floor the bound is derived from, measured |

### The capability arm, and the thresholds behind it

`the_deepest_by_value_constructors_fit_on_a_tight_stack` spawns a 1 MiB thread
and runs BOTH deepest public chains: the three-deep `Result<Self>` encoder
constructor nest, and the four-deep `Result<Encoded>` key-frame chain. A
regression **aborts the process** rather than costing invisible margin.

Measured stack need, **debug** build, 64×64, five runs per point, identical
every time (these are deterministic thresholds, not a distribution):

| stack | `Av1Encoder` pyramid ctor | `encode_key_frame` |
|---|---|---|
| 65536 | overflow | overflow |
| 131072 | overflow | overflow |
| 262144 | **overflow** | overflow |
| 524288 | ok | overflow |
| 655360 | ok | overflow |
| 720896 | ok | **overflow** |
| 786432 | ok | ok |
| 1048576 | **ok** | **ok** |

Reproduce any row with `EC_AV1_TIGHT_STACK_BYTES=<bytes> cargo test -p ec-av1
--lib -- the_deepest_by_value_constructors_fit_on_a_tight_stack`.

### Mutation proof — both mutations, both reverted

**M1 — unbox what lane-av1enchang boxed** (a real revert, not a synthesised
one). `DpbSlot::cdfs: Box<CdfSnapshot>` → `CdfSnapshot` in `encoder.rs`, plus
the one construction site.

```
Av1Encoder is 139960 bytes, over the 32768-byte by-value stack budget (4.27x).
  ... returned by value from Av1Encoder::new / with_speed / with_pyramid /
  with_rate_target / with_pyramid_and_rate_target, all `Result<Self>`,
  nesting three deep ...

assertion `left == right` failed: Av1Encoder is 139960 bytes,
  the inventory records 18168

thread '<unknown>' has overflowed its stack
fatal runtime error: stack overflow, aborting
(signal: 6, SIGABRT)   exit 101
```

Three arms fired: the bound **by name**, the inventory with both numbers, and
the capability test as a process abort. Reverted; `git diff` empty; 7/7 green.

**M2 — grow `Encoded` past the bound** (synthesised, and labelled as such:
`Encoded` has no prior boxing to revert, so the honest mutation is to add a
field of the same shape that produced the class — a 64 KiB inline payload).
Three construction sites patched.

```
Encoded is 96264 bytes, over the 32768-byte by-value stack budget (2.94x).
  ... returned by value from encode_key_frame / encode_key_frame_with_modes /
  encode_key_frame_at_size -> Result<Encoded>, nesting four deep ...

assertion `left == right` failed: Encoded is 96264 bytes,
  the inventory records 30728

fatal runtime error: stack overflow, aborting   (signal: 6, SIGABRT)
```

Reverted; `git status` clean apart from the two intended files.

**What is proven and what is not.** Both mutations prove the bound row and the
inventory row are live reads of the real `size_of`, not constants, and that
the capability arm reaches the process abort. They do **not** prove the 1 MiB
tight-stack threshold has margin against a future rustc's frame layout — only
that a large regression aborts rather than passes quietly. The 4.4× ratio in
§1 is the honest statement of that residual risk.

---

## 4. THE 64 MiB CAP — what still needs it

### First: the cap is inert for anything but cargo

`.cargo/config.toml` sets `[env] RUST_MIN_STACK = "67108864"`. Cargo applies
`[env]` only to processes **cargo spawns**. Running the test binary directly
does not get it. Measured both ways on the same binary:

```
RUST_MIN_STACK unset    -> test thread stack 2101248 bytes
RUST_MIN_STACK=2097152  -> 2101248
RUST_MIN_STACK=8388608  -> 8392704
RUST_MIN_STACK=67108864 -> 67112960
via `cargo test` (repo [env] in effect) -> 67112960
```

So the cap does what it claims for cargo-driven runs, and a library consumer
who links this crate gets the 2,101,248-byte default.

### Second: the comment in `.cargo/config.toml` is now inaccurate

It reads:

> The AV1 rate-target tests keep several frames' worth of encoder state on the
> test thread's stack; the 2 MB default overflows in debug builds (release
> passes).

**"Several frames' worth of encoder state" is not on the stack.** The state it
means is `Av1Encoder`'s `pending: Vec<(u64, Picture)>`, `ready:
Vec<(u64, Picture)>` and `held: VecDeque<Picture>` — all heap. A source scan
for a large fixed-size stack local in the encoder path finds exactly one, in
`film_grain.rs`:

```
crates/ec-av1/src/film_grain.rs:26:  const GAUSSIAN_SEQUENCE: [i32; 2048] = [
```

which is a `const`, i.e. in `.rodata`, not a frame.

The claim was true **before** lane-av1enchang: a 139,960-byte `Av1Encoder`
returned by value through a three-deep `Result<Self>` nest is ~420 KB of live
stack before the encode path adds its own ~790 KB, and that does not fit in
2 MiB with the rest of the chain. lane-av1enchang's boxing removed the
dominant term.

**I am not editing `.cargo/config.toml` in this branch**, as chartered. The
recommended replacement comment, once the numbers below are accepted, is:

> The AV1 encoder's by-value returns (`Av1Encoder` 18168 B x a 3-deep
> `Result<Self>` nest, `Encoded` 30728 B x a 4-deep chain) are gated by
> `stack_budget.rs` against a 32 KiB per-type budget, and the deepest chain
> measures 786432 bytes in a debug build. This 64 MiB cap is no longer what
> keeps them alive; it is a blanket headroom for test threads, and lowering it
> is a separate change that needs its own per-test evidence.

### Third: the measured answer — nothing in ec-av1 needs 64 MiB

The cap is 64× the deepest measured need.

| what | measured stack need (debug) | at 2 MiB | at 64 MiB |
|---|---|---|---|
| `encode_key_frame`, 64×64 | 786,432 B | **2.66× margin** | 85× |
| pyramid `encode_sequence`, 640×384 | 917,504 B | **2.13× margin** | 68× |
| `Av1Encoder` pyramid ctor nest | 262,144 B | **8.0× margin** | 256× |
| the rate-target test's own worker body (see below) | 983,040 B | **2.13× margin** | 68× |

The last row is the one the cap's comment names.
`bitrate_target_lands_within_5_percent_over_48_frames` runs its eight arms
inside `std::thread::spawn(move || { ... })` (encoder.rs:3602) — a thread
with **no** `stack_size`, so it gets the runtime default, exactly the case a
library consumer hits. Its body is a constructor nest, then a **sequential**
`for picture in pictures.iter()` loop of `encode_frames`, then `flush`. Frames
do not nest, so the peak is one frame's chain, not 48 of them. I measured that
exact body at 640×384 in a debug build:

```
STACK rate 640x384 1048576 -> ok
STACK rate 640x384  983040 -> ABORT
```

983,040–1,048,576 B. A 2 MiB thread has 2.13× margin. **The 64 MiB cap is
64× what this test needs.**

*(Stack depth here is set by call nesting and partition recursion depth, which
is bounded by block size and not by picture content, so a real clip rather
than the grey frames I measured should need the same. That part is
[INFERENCE]; the release run of the real clip in §5 is the empirical check.)*

### What I could not measure, and why

- **`bitrate_target_lands_within_5_percent_over_48_frames` in DEBUG.** It is a
  ~7 hour test (the crate's own comment: the whole debug lib suite is 24341s
  and this is the largest single item), and the charter says heavy suites run
  on the VPS fleet, not locally. What I measured instead is its worker body in
  isolation (§above), which is the same call chain without the 48-frame
  runtime. The command to settle it on a fleet host is
  `env -u RUST_MIN_STACK cargo test -p ec-av1 --lib -- bitrate_target_lands_within_5_percent_over_48_frames --exact`.
- **The sibling crates' own test suites** were not run at 2 MiB. The
  measurement that matters for them is the per-type stack sweep in §1, and
  that is complete: the only type in the workspace that would be in danger on
  a 2 MiB thread is `ec_opus::Encoder`, and it survives 384 KiB.

### Recommendation

**Lower the cap, but not in this lane, and not to 2 MiB blind.** Dropping
`RUST_MIN_STACK` to `2097152` is supported by every measurement above for
ec-av1's encoder paths, but it is a workspace-wide change affecting 30 crates,
and this lane has per-test evidence for ec-av1 only. The two-step that the
evidence supports:

1. Take `ec_opus::Encoder` (87,024 B, §1) down first. At 384 KiB of live
   stack it is 2.66× over a 32 KiB budget and is the one type in the
   workspace whose constructor chain could plausibly meet a small thread.
2. Then run each crate's suite once at `RUST_MIN_STACK=2097152` on the fleet,
   in per-test processes so a stack overflow abort is attributable rather
   than swallowing the rest of the run, and lower the cap to whatever the
   largest surviving requirement turns out to be.

---

## 5. WHAT IS NEXT IN THIS CLASS (not done here)

**`Encoded` should have its two `CdfSnapshot` fields boxed.** 30,464 of its
30,728 bytes (99.2%) are `start_cdfs` and `next_cdfs`, both
`CdfSnapshot(Cdfs)` inline. lane-av1enchang boxed exactly this payload in the
DPB slot for exactly this reason and left this copy alone. Boxing both would
take `Encoded` from 30,728 to ~264 bytes, drop the deepest by-value chain's
multiplier by 116×, and take the key-frame tight-stack threshold from 786,432
down by roughly that much.

That is a layout change with no bitstream effect — the same change lane-av1enchang
made and measured byte-identical — but it touches `encode.rs` construction
sites that other lanes have open, so it is not this lane's to land.

**`ec_opus::Encoder` at 87,024 bytes** is the largest instance of the class in
the workspace and lives in another crate entirely.

---

## 6. ACCEPTANCE EVIDENCE

### Release run

```
$ CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1stacksweep EC_NOMEMGUARD=1 \
    cargo test --release -p ec-av1 --lib -- encode round_trip \
    --skip bitrate_target_lands_within_5_percent_over_48_frames
```

test result: ok. 108 passed; 0 failed; 40 ignored; 0 measured;
             650 filtered out; finished in 200.51s

Wall time: 224.72 seconds
```

All 40 ignored are the crate's pre-existing wall-measurement gates
(`tile_search_wall_1080p`, `filter_stage_wall_4k`, ...), which carry their own
"run it with --ignored" reasons and are not affected by this lane. The 650
filtered out are the decode/gate families outside the `encode` / `round_trip`
substring filters.

Run in RELEASE because the debug encoder suite is hours; run with the repo's
64 MiB cap in effect, i.e. exactly how CI runs it.

### The new gate, debug

```
$ cargo test -p ec-av1 --lib -- stack_budget --nocapture
running 7 tests
test stack_budget::tests::every_by_value_type_fits_the_stack_budget ... ok
test stack_budget::tests::the_measured_inventory_is_accurate ... ok
test stack_budget::tests::the_inventory_scan_is_not_vacuous ... ok
test stack_budget::tests::every_by_value_module_is_scanned ... ok
test stack_budget::tests::test_threads_get_the_stack_they_are_given ... ok
test stack_budget::tests::the_deepest_by_value_constructors_fit_on_a_tight_stack ... ok
test stack_budget::tests::every_by_value_public_return_is_in_the_inventory ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 791 filtered out; finished in 0.05s

TESTTHREADSTACK default tid=2934549 region=[7f7063fff000,7f7068000000) size=67112960 alone=false
  (2101248 measured with the repo cap lifted, 67112960 with it)
TIGHTSTACK 1048576 bytes: Av1Encoder=18168 Encoded=30728 bytes, both chains returned
```

### Files changed

```
crates/ec-av1/src/stack_budget.rs   (new, 7 tests)
crates/ec-av1/src/lib.rs            (+1 line: `mod stack_budget;`)
lanes/av1stacksweep.report.md        (this file)
```

Not touched: `decode.rs`, `.cargo/config.toml`, any codec logic.
