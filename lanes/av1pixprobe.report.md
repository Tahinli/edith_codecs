# lane-av1pixprobe — the two `av1cheapwins` §C "land after rebase" items

Base: `1d47ff54` (main). Two independent pieces, landed:

* **A** — `EC_AV1_PIXPROBE=x,y`, the env-gated one-sample write probe from
  `lane-av1-ibcwrite` @ `25f5c729`, **re-anchored by hand** onto main's
  reconstruction-store sites (the branch's two mc hunks no longer attach: the
  mc stores live in `reconstruct_mc_rect` now, and its block extent is `w`,
  not `side`).
* **B** — the substance behind `lane-av1-c10`: ancestor commit **`295dc3fe`**'s
  byte-identical `u8` cast move in `diffwtd_mask` **plus** its 8/10-bit libaom
  transcription gate `mc::libaom_transcription::compound_pipeline_matches_the_libaom_transcription_at_8_and_10_bits`
  (adapted, not cherry-picked — see "B" below).

Defect **C** is recorded, not chased — and it does **not** reproduce on merged
main; see "C".

`cargo check --all-targets -p ec-av1` clean, gate green, rung fires and is
inert when unset.

---

## A — the write probe, re-anchored

### Why the cherry-pick does not apply

`git show 25f5c729 -- crates/ec-av1/src/decode.rs` has four store hunks. Two of
them (the intra pair) still attach: `reconstruct_rect` and `reconstruct` both
exist on main with the same
`self.data.to_mut()[(y + row) * self.width + x + col] = sample;` line inside
the same `let sample = (base + residual[idx]).clamp(...)` store. The two mc
hunks do **not**: they were written against the pre-restructure
`mc_copy`/`mc_add` loops; on `1d47ff54` both stores are inside
`PlaneBuf::reconstruct_mc_rect` (`decode.rs:36454` onward), which
lane-perf4/lane-recoparse reshaped (row-at-a-time `pred`/`out` slice pairs, an
early `residual.is_empty()` return for the prediction-only path, and a
`w`/`h` block extent with `side` demoted to the prediction stride). So the
loops were re-located by grep and re-written in place.

Two deliberate deviations from the branch's text, both forced by main's shape:

1. `pix_write("mc_copy", x, y, w, …)` / `pix_write("mc_add", x, y, w, …)` — the
   branch passed `side`. In `reconstruct_mc_rect` `side` is the *prediction
   stride*, not the block width (a rect block is `w` wide and `side`-strided),
   so reporting `side` in the `block=(bx,by)+SIDE` field would misreport the
   block extent for every rectangular inter block. `w` is what the field means.
2. **Formatting.** The branch wrote the two intra `pix_write` calls as single
   long lines. This repo does have a formatting hook, but not where I first
   looked: `core.hooksPath` is `/home/tahinli/omp/omp/agent/hooks/git`, so
   `$(git rev-parse --git-common-dir)/hooks/pre-commit` is absent and says
   nothing. The commit-time rustfmt reflowed the four call sites and the
   `pix_write` signature across lines (semantic-only; the diff grew from the
   branch's +33/-4 to +64/-4 and every extra line is reflow). Accepted, not
   reverted.

### The landed diff (`decode.rs +64/-4`; the +31 over the branch's +33 is pure reflow)

(shown in the branch's single-line form for readability; as committed the
`pix_write` signature and the two intra call sites are rustfmt-reflowed, which
is where the extra 31 lines are — no token changed)

```
  @@ -20395,6 +20395,31 @@ struct PlaneBuf<'a> {
  +// lane-av1pixprobe: env-gated one-sample write probe -- `EC_AV1_PIXPROBE=x,y`
  +// prints one PIXWRITE line per reconstruction store landing on that sample,
  +// naming the store site, the value it replaces and the one it leaves. Re-anchored
  +// from `lane-av1-ibcwrite` @ 25f5c729 onto the main-shape sites (the two mc stores
  +// now live in `reconstruct_mc_rect`).
  +static PIXPROBE_ON: std::sync::LazyLock<bool> =
  +    std::sync::LazyLock::new(|| std::env::var_os("EC_AV1_PIXPROBE").is_some());
  +static PIXPROBE: std::sync::LazyLock<Option<(usize, usize)>> = LazyLock::new(|| { … });
  +#[inline]
  +fn pix_write(site: &str, bx: usize, by: usize, bside: usize, x: usize, y: usize, prev: u16, val: u16) {
  +    if *PIXPROBE_ON && Some((x, y)) == *PIXPROBE {
  +        eprintln!("PIXWRITE {site} block=({bx},{by})+{bside} px=({x},{y}) prev={prev} val={val} at {}", Location::caller());
  +    }
  +}
  @@ -20599,6 +20624,7 @@   (reconstruct_rect store)
  +                pix_write("reconstruct_rect", x, y, bw, x + col, y + row, self.data[…], sample);
  @@ -20745,6 +20771,7 @@   (reconstruct store)
  +                pix_write("reconstruct", x, y, side, x + col, y + row, self.data[…], sample);
  @@ -36485,8 +36512,10 @@  (reconstruct_mc_rect, residual.is_empty() path)
  -                for (o, &p) in out.iter_mut().zip(pred) {
  -                    *o = i32::from(p).clamp(0, max) as u16;
  +                for (col, (o, &p)) in out.iter_mut().zip(pred).enumerate() {
  +                    let v = i32::from(p).clamp(0, max) as u16;
  +                    pix_write("mc_copy", x, y, w, x + col, y + row, *o, v);
  +                    *o = v;
  @@ -36498,8 +36527,10 @@  (reconstruct_mc_rect, dense-residual path)
  -            for ((o, &p), &r) in out.iter_mut().zip(pred).zip(res) {
  -                *o = (i32::from(p) + r).clamp(0, max) as u16;
  +            for (col, ((o, &p), &r)) in out.iter_mut().zip(pred).zip(res).enumerate() {
  +                let v = (i32::from(p) + r).clamp(0, max) as u16;
  +                pix_write("mc_add", x, y, w, x + col, y + row, *o, v);
  +                *o = v;
```

Inertness is structural, not incidental: `PIXPROBE_ON` is a `LazyLock<bool>` over
`std::env::var_os(..).is_some()`, so with the variable unset the whole body
compiles to one atomic load per store and the `eprintln!` is behind a `false`
that the branch's own envflags note (lane-perf1) shows is free. Nothing in the
crate calls `set_var` (`grep -rn set_var crates/ec-av1` = 0 hits), so the
snapshot cannot change under a run.

### Proof it fires — the pinned fixture, re-encoded here

The `lane-av1ibcwrite` / `lane-av1ibcpix` recipe, re-run verbatim
(`testsrc2=size=320x180:rate=25 -t 0.2 -vf tile=2x2` y4m →
`aomenc --passes=1 --end-usage=q --cpu-used=0 --lag-in-frames=0 --kf-max-dist=1
--limit=1 --threads=1 --tile-columns=0 --min-partition-size=8
--max-partition-size=32 --sb-size=64 --tune-content=screen --enable-intrabc=1
--enable-palette=0 --cq-level=20 --enable-tx-size-search=1
--enable-rect-partitions=0 --enable-1to4-partitions=0 --max-partition-size=64
--obu`):

```
$ sha256sum out.obu
bc699af4a6e7c1c5657f041e8cbdf2d3d899ddb80ec2a549a950b7cebb5b41bd  out.obu
```

— byte-identical to the sha both parked reports pin.

Set:

```
$ EC_AV1_PIXPROBE=296,128 decode_probe out.obu
PIXWRITE reconstruct block=(288,128)+32 px=(296,128) prev=0 val=170 at crates/ec-av1/src/decode.rs:20418:13
PIXWRITE reconstruct block=(288,128)+16 px=(296,128) prev=0 val=167 at crates/ec-av1/src/decode.rs:20418:13
PIXWRITE reconstruct block=(288,128)+16 px=(296,128) prev=0 val=15 at crates/ec-av1/src/decode.rs:20418:13
OK: 1 frames decoded, 640x360
```

Three stores, one per plane (luma `val=170`, U `val=167`, V `val=15`) — the
same three-stores-one-per-plane shape the branch reported, at the sample the
branch named. Unset:

```
$ env -u EC_AV1_PIXPROBE decode_probe out.obu 2>&1 | grep -c PIXWRITE
0
```

Build:

```
$ CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1pixprobe cargo check --all-targets -p ec-av1
    Finished `dev` profile [optimized + debuginfo] target(s) in 1.76s     (exit 0)
```

---

## B — `diffwtd_mask`'s `u8` cast move + its 8/10-bit gate

### The cast move is byte-identical — argued from the clamp, not asserted

```rust
let diff = round2((pred0[i] - pred1[i]).abs(), round);
let m = (38 + diff / 16).clamp(0, 64) as u8;   // was: (… .clamp(0, 64); with `m as u8` at the use
mask[i] = if inv { (64 - m) as u8 } else { m as u8 };
```

`clamp(0, 64)` on an integer leaves `m ∈ {0,…,64}` — 65 values, all
representable in `u8` — so `as u8` is value-preserving and the two forms bind
the *same* integer. Every use is a narrowing of a value already in range:

* forward: `m as u8` (old) vs `m` already `u8` (new) — identical byte.
* inverse: `64 - m` is in `1..=64` for `m ∈ 0..=64`, so it is non-negative and
  `u8`-representable; `64u8 - m_u8` cannot wrap, and `(64 - m_i32) as u8` is the
  same value. Identical byte.

The only behavioural difference between the two bindings is the *domain* of the
subtraction (`u8` vs `i32`), and since neither operand can leave `[0,64]` there
is no value for which the two domains differ. The mask array is `&mut [u8]`, so
the stored bytes are the same in both. This is a type-tightening only: it moves
the range proof to the binding and deletes two now-redundant casts.

### The gate — landed, and proven non-vacuous at BOTH depths

`mc::libaom_transcription::compound_pipeline_matches_the_libaom_transcription_at_8_and_10_bits`
(213 lines, from `295dc3fe`) pins the whole compound path — two
`predict_compound_intermediate` taps through `combine_compound` (dist-wtd 9/7
and simple 8/8), `diffwtd_mask` both inverses, and `blend_masked_compound` —
against faithful transcriptions of `av1_highbd_dist_wtd_convolve_2d_c`,
`diffwtd_mask_d16` and `aom_highbd_blend_a64_d16_mask_c`, over **all 256 phase
pairs at 8 and 10 bits**, with no aomenc stream and no fixture. That is
precisely the 10-bit coverage the `(bd - 8)` term in `diffwtd_mask` landed
without (cwarp-r1's `accepted` residue).

Adaptation, because main's `blend_masked_compound` signature has drifted: the
branch's `subsampled: bool` is now the plane's own `subw`/`subh` shift pair, so
the call is

```rust
blend_masked_compound(&p0, &p1, &mask, 8, 8, 8, 0, 0, &mut blended, fctx);
```

(luma, `(subw, subh) = (0, 0)`), and the transcription is unchanged — the
branch's transcription was already the `subw == subh == 0` luma case. Every
other identifier and signature (`predict_compound_intermediate`,
`combine_compound`, `InterpFilterKind::Regular.tables()`, `FrameCtx::new`,
`set_bit_depth`, `REF_NO_SCALE`) still matches main unchanged. One
`#[allow(clippy::too_many_arguments)]` was added to the transcription helper,
which main's lints require and the branch's tree did not.

Run:

```
$ cargo test -p ec-av1 --lib compound_pipeline_matches_the_libaom_transcription
running 1 test
test mc::libaom_transcription::compound_pipeline_matches_the_libaom_transcription_at_8_and_10_bits ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 764 filtered out; finished in 0.06s
```

Non-vacuity, two counterfactuals on `diffwtd_mask`'s own `round` line, each
reverted immediately:

| mutation | result |
|---|---|
| `… - round_delta(bd) + 1` (bites at 8-bit) | `FAILED … assertion left == right failed: diffwtd bd 8 inv false phase (0,0)` |
| `INTER_POST_ROUND + bd.saturating_sub(8) * 2 - round_delta(bd)` — **zero at 8-bit, nonzero at 10-bit only** | `FAILED … assertion left == right failed: diffwtd bd 10 inv false phase (0,0)` |

The second is the load-bearing one: it is inert at 8-bit and only the 10-bit arm
catches it, so the 10-bit coverage is real, not a duplicate of the 8-bit arm.
Both reverted; the green run above is the reverted tree.

---

## C — the defect the branch named: RECORDED, NOT INVESTIGATED

**Not investigated by contract.** Re-measured only to the extent needed to say
whether the recorded numbers still describe merged main, which changes what a
charter must start from. That re-measurement is itself the most important line
in this section.

### The recorded defect (verbatim from `lane-av1ibcwrite.report.md`, `25f5c729`)

> An 8x8 intra-BC leaf at px **(296, 128)** stores **prediction 170 + residual
> 19 = 189** where the oracle's single all-zero `TX_32X32` unit stores **170**.
> First value divergence upstream: at mi **(row 32, col 72)** (the owning 32x32
> luma TU at px (288,128), the 64x64 at x 256..320 / y 128..192) ours reads
> `skip=1` where the oracle reads `skip=0`; the oracle codes that quadrant
> `PARTITION_NONE` as one 32x32 with `all_zero=1`, ours splits it into 8x8/16x16
> leaves, and from that symbol the arithmetic streams desync. Our desynced 8x8
> leaf at (296,128) then reads `all_zero=0` with one DC level 2 → dequant 19 →
> the 189 store. Oracle output at that sample: **170**; the class repeats **301
> times** per frame (independent roots with a clean left/above/above-left),
> 127,862 differing samples (Y 84,510 / U 21,005 / V 22,347), rows y 128..359.
> Fixture: the R5 encode above, sha256
> `bc699af4a6e7c1c5657f041e8cbdf2d3d899ddb80ec2a549a950b7cebb5b41bd`. The
> named home is the partition-context / leaf8 intrabc neighbour stamping
> (`leaf8_intrabc_hits` territory), upstream of the whole decode tail.

### What merged main does with the same fixture today

The branch tip `37a64d99` is **not** an ancestor of main; its merge-base with
`1d47ff54` is `7f863c5f`, i.e. the measurement tree is **439 commits behind**
main with one commit of its own on top. Re-encoding the recipe reproduces the
pinned sha byte-for-byte, and on this tree:

```
$ dump_yuv out.obu /tmp/ours ; ffmpeg -i out.obu -pix_fmt yuv420p -f rawvideo ref.yuv
$ cmp ours.f0.yuv ref.yuv
$ # python: total differing samples vs ffmpeg: 0    Y differing: 0
$ #         Y(296,128): ours 170  ref 170
```

**The frame is byte-exact against ffmpeg on `1d47ff54`**, so the 189 store is
**not reproducible on merged main** for this fixture: Y(296,128) is 170 in both
frames, and the probe above prints `val=170` for the luma plane at that
coordinate. Luma still contains four samples equal to 189 in total — at
**(70,218), (70,219), (70,223), (389,225)** — but they match the oracle
byte-for-byte, so they are picture content, not residues of this class.

### Hand-off row

> **OPEN, un-reproduced on merged main.** Class: partition-tree divergence
> around skipped intrabc leaves (entropy desync, not a write bug — the
> reconstruction arithmetic is correct and the write site is exonerated). The
> original numbers: 8x8 intrabc leaf at px **(296,128)** storing
> **pred 170 + res 19 = 189** where the oracle's all-zero `TX_32X32` stores
> **170**; first divergent symbol at mi **(32,72)** — `skip=1` vs `skip=0`,
> `PARTITION_NONE` vs an 8x8/16x16 split; **301** independent roots per frame;
> **127,862** differing samples (Y 84,510 / U 21,005 / V 22,347), y 128..359;
> fixture sha256 `bc699af4a6e7c1c5657f041e8cbdf2d3d899ddb80ec2a549a950b7cebb5b41bd`
> (recipe reproduced here, byte-identical). **On `1d47ff54` the fixture decodes
> byte-exact vs ffmpeg (0 differing samples)** — the measuring tree `37a64d99`
> is 439 commits stale (merge-base `7f863c5f`), so either a later lane closed
> it on this input or the tree it was measured on is not the tree it was
> reported against. **A charter must start by re-bisecting on merged main, not
> by trusting these coordinates**; `EC_AV1_PIXPROBE=x,y` (this branch) is the
> re-measurement instrument, and the owning area named by the original report
> is the partition-context / leaf8 intrabc neighbour stamping.

**Not investigated:** I did not trace the partition path, did not run the
oracle's `EC_ISTEP`/`EC_TRACE_COEFF` rungs against main, did not try to find
which of the 439 commits changed the outcome, and did not confirm or deny that
the class still exists anywhere. The 301-root / 127,862-sample figures are the
branch's, quoted for provenance; they are **not** re-measured numbers.

---

## What I deliberately did not do

- **Did not cherry-pick.** Both commits were re-anchored/re-adapted by hand;
  `git cherry-pick` was never run against either, because both would conflict
  (`decode.rs` sites moved; `blend_masked_compound`'s signature changed).
- **Did not land `lane-av1c10` @ `a2806383`** (the report-only disclosure
  correction at that tip). §C triaged it "record only" and its content
  duplicates this report; only the ancestor `295dc3fe` substance landed.
- **Did not land `lanes/av1ibcwrite.report.md` / `lanes/av1c10.report.md`**
  verbatim. The ibcwrite report's "the write is named" narrative is superseded
  for merged main by the re-measurement above; C quotes it as a record instead.
- **Did not investigate the C defect.** See the hand-off row: one fixture
  re-encode + one `cmp` against ffmpeg to establish non-reproduction, nothing
  further.
- **Did not touch the main checkout.** It is clean after every batch; the
  absolute-path discipline held (see below).
- **Did not run any full suite.** Scoped to one named test in one crate, one
  example build, one one-frame fixture probe. Box was loaded.
- **Did not run rustfmt myself.** The commit hook reflowed the new lines
  (`core.hooksPath=/home/tahinli/omp/omp/agent/hooks/git`); the reflow is
  semantic-only and was left in place rather than fought.
- **Did not add a gate for the pixprobe rung itself** (e.g. a
  fires-when-set assertion). A test that would have to spawn a process with the
  env set is out of proportion to an inert debug rung, and the repo's
  convention for these is the example-binary probe quoted above.
- **Did not push.**

### One process note worth carrying forward

The `edit` tool resolved a **relative** path header against the session's cwd
(the main checkout), not the worktree, and silently wrote the first insertion
into `main`'s `decode.rs`. Caught immediately by
`git status --porcelain` in the main checkout showing
` M crates/ec-av1/src/decode.rs`, reverted with `git checkout --`, and every
later edit used an absolute `/home/tahinli/.cache/wt/av1pixprobe/…` path. The
contract's "relative edits land in the main checkout" is not a warning about a
*different* worktree arrangement — it is literally what happens, on the first
edit, with a relative path.
