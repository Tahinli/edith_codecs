# refute-av1-w3a — an independent refutation pass over the six byte-exactness merges of 2026-09-30

Read-only audit. Nothing was fixed, no merge was re-opened, no push, no rustfmt.
Scratch worktree `~/.cache/wt/Cem2-3` (branch `lane-Cem2-3`, from `main` = `81a21c7d`)
was created, every historical state was measured inside it, and it was removed
afterwards. `git status --porcelain` on the primary checkout is EMPTY. `main` has
since advanced to `52c62c15`; every number below is pinned to an explicit SHA, so
none of it moves.

## 0. Method, in the order it mattered

**Fixture bytes.** Every cell I measured is the cell the report pins:

| cell | sha256 (first 16) | bytes | committed as |
|---|---|---|---|
| `W_intrabc.obu` | `0aad0d6fdd236557` | 130 320 | `crates/ec-av1/fixtures/W_intrabc.obu` — same sha |
| `X_intrabc_tiled.obu` | `e7c0c60af1a61531` | 131 696 | `fixtures/X_intrabc_tiled.obu` — same sha |
| `Y_intrabc_10b.obu` | `ce84d7cb4cfadf3f` | 202 281 | `fixtures/Y_intrabc_10b.obu` — same sha |
| `regress_444/ll444_c.obu` | `999a41d88a092840` | 512 220 | `fixtures/ll444_c_altref_leaf8_warp.obu` — same sha |
| `probe/ll422_allintra.obu` | `d56f6b655743897a` | 43 255 | probe cell, not committed |
| `probe/ll422_noibc.obu` | `c8559205ecf398fa` | 460 682 | probe cell, not committed |
| `probe/ll420_allintra.obu` | `c5ea0690a10e38b6` | 32 544 | probe cell, not committed |
| `probe/ll444_allintra.obu` | `47669206ad085842` | 60 840 | probe cell, not committed |

**Two legitimate bases, and telling them apart is the whole game.**

* *decode order* — `EC_AV1_FINAL_DUMP` on both sides (`aomdec` rung 12, and
  `stream.rs:2210`), Y then U then V, u8 / u16-LE by the header's bit depth,
  **every** picture including hidden alt-refs. This is what I used for every
  per-frame and per-plane table below.
* *display order* — `aomdec --rawvideo` against our `dump_yuv`, **shown frames
  only**, hidden alt-refs dropped. This is what several reports' headline
  totals are on.

On `ll444_c` the two disagree: display **317** vs decode **397**, the gap being
the hidden picture's 80. A total quoted in a decode-order table that is really
the display-order number is a labelling error, not a wrong fix. I always report
both.

**Comparator (`~/.cache/cem23/cmp.py`), written from scratch, not a lane's.**
Geometry is an explicit argument, never inferred from file size; a file-size
mismatch and an empty glob are both hard errors; per-plane, per-frame; counts
differing **samples** (u16 pairs for HBD). It reads `<prefix>.f<N>` from BOTH
sides — the failure class that made `lane-av1422anom` withdraw an "18/18
byte-exact" is a comparator that loads one side and compares it with itself, and
that is structurally impossible here.

**Comparator liveness, per cell.** For all eight primary cells I flipped **one**
oracle byte in **each** of Y, U and V at one frame and re-ran:

| cell | flipped frame | Y | U | V | other frames |
|---|---|---|---|---|---|
| `W_intrabc` | f1 | +1 | +1 | +1 | unchanged |
| `X_intrabc_tiled` | f1 | +1 | +1 | +1 | unchanged |
| `Y_intrabc_10b` | f1 | +1 | +1 | +1 | unchanged |
| `ll422_allintra` | f0 | +1 | +1 | +1 | n/a (1 frame) |
| `ll422_noibc` | f1 | +1 | +1 | +1 | unchanged |
| `ll420_allintra` | f0 | +1 | +1 | +1 | n/a |
| `ll444_allintra` | f0 | +1 | +1 | +1 | n/a |
| `ll444_c` | f1 | +1 | +1 | +1 | unchanged |

Exactly +1 on the plane and frame flipped, +0 everywhere else, every time. The
zeros below are live comparisons.

**Reaching 4:2:2.** The one-line `stream.rs` header guard was patched out once,
saved as a diff, and `git apply`-ed after every `git reset --hard`; the patch
does NOT apply at every historical SHA (the surrounding comment text shifts), so
each state was checked with `git status --porcelain` before its build was
trusted. No bypass, probe or counter of mine is in any commit.

**State ladder.** One worktree, one `CARGO_TARGET_DIR`, `git reset --hard <sha>`
→ `git apply bypass` → build → run. Each lane was measured on **its own**
before/after pair, which is the branch parent → branch tip, not the merge's
first-parent → merge. That distinction matters: merge `ac00e9fb` also carried
lane-stackbox's 67-line `stream.rs` + 158-line `decode.rs` change, so measuring
`d202b8b4 → ac00e9fb` made a false refutation appear (see claim 5).

---

## 1. `3698787d` lane-av1422lpf — **CONFIRMED** (two sub-claims refuted, one not reproduced)

**Confirmed — all three panics gone.** At the lane's declared base `aef4fa67`
with the bypass, `W_intrabc`, `X_intrabc_tiled` and `Y_intrabc_10b` all die with
exit 101 at `crates/ec-av1/src/decode.rs:37565:24` — the exact line the report
quotes. At the pre-merge main `18643bb5` they all panic at `:37617` with
`range end index 20 out of range for slice of length 16`. At `3698787d` all three
decode **17 of 17** pictures and exit 0. `ll422_noibc` also panicked before
(`range end index 1028 out of range for slice of length 1024`) and decodes 16/16
after — a fourth cell the report did not claim.

**Confirmed — the report's own "not byte-exact" honesty.** At `3698787d`, W, X
and Y are each 0/17 exact. Nothing was overstated.

**Confirmed — zero regressions, sample-exact.** `ll420_a/b/c/d` and
`ll444_a/b` are **16/16 byte-exact on all three planes at both `18643bb5` and
`3698787d`**; `ll444_c` is 8/18 exact with identical counts
(`Y 0 / U 185 / V 212`) at both. Display order, `ll444_c` is **7 of 16 shown
frames exact** at both — the "DIVERGES 7/16" row reproduces exactly.

**REFUTED — §4.1's symptom column.** "`W_intrabc` → `a Golomb tail longer than
this decoder reads` at mi (16,52)"; "`X_intrabc_tiled` → same at mi (8,64)".
At `3698787d` with the bypass, W and X decode all 17 pictures and **print no
refusal message at all**. The quantity that kills it: 17 of 17 pictures emitted,
0 refusal lines. (`lane-av1422llintra` §1 retracted this itself; I confirm the
retraction independently rather than taking it on trust.)

**REFUTED — §5's `not_done` line.** "Decode-order (rung 12) comparison was not
run on the three cells — they do not decode to completion, so there is nothing
to zip against the oracle's per-frame dumps." They do decode to completion. I
ran rung 12 on both sides for all three, 17 frames per side, 153 600 B per frame
(Y 76 800 + U 38 400 + V 38 400 for the 8-bit cells, 307 200 B for the 10-bit
cell), and the oracle's 17 frame files match those expected sizes exactly. The
stated blocker does not exist.

**NOT REPRODUCED — §1.1's "site 1 is NOT 4:2:2-specific".** The report's
headline counter-evidence is a 4:2:0 panic:
`panicked at decode.rs:37574:24: range end index 1028 out of range for slice of
length 1024` on `ll420_a.obu`, argued from a 64x128 block's 32x64 chroma plane
block. On the pinned `regress_420/ll420_a.obu` at `aef4fa67` there is **no
panic** (exit 0), and all four `ll420_*` cells decode with exit 0 at
`aef4fa67`; at `18643bb5` and `3698787d` they are 16/16 byte-exact. The one place
I could produce a `1028 / 1024`-shaped panic is `ll422_noibc`, a **4:2:2** cell,
at `18643bb5`. The cell the report used is pinned by no sha, so the claim cannot
be reproduced from anything committed, and its evidence for "not 4:2:2-specific"
is unsupported. The per-site arithmetic may still be right; the demonstration
does not stand.

**Untested.** The 33-cell diff of §3.2/§3.3 beyond the seven lossless controls
(the lossy 4:2:2 corpus) — those cells are unchanged at `3698787d` by
construction, and the same corpus is measured in §7 below at the llinter2 pair.

---

## 2. `9367642a` lane-av1444altref — **CONFIRMED** (one §1 table entry refuted)

**Confirmed, and the attribution is clean.** The merge's first parent is
`a20aba01`; the lane's own base is `3698787d`. On **both**:

| state | `ll444_c` decode order | Y / U / V |
|---|---|---|
| `3698787d` (lane base) | 8/18 exact | 0 / 185 / 212 |
| `a20aba01` (merge parent) | 8/18 exact | 0 / 185 / 212 |
| `9367642a` (merge) | **18/18 exact** | **0 / 0 / 0** |
| `2005a35d`, `5c216458`, `ac00e9fb`, `81a21c7d` | 18/18 exact | 0 / 0 / 0 |

"Exact before this commit" cannot masquerade as "this commit fixed it": the
commit is what moves the cell, and nothing else in the chain does.

**Confirmed — the headline 317, exactly, on its own basis.** Display order
(`aomdec --rawvideo` vs `dump_yuv`, 16 shown frames, 320x240 4:4:4 8-bit) at
`3698787d`: **`Y 0 / U 147 / V 170`, total 317.** Not "close to 317" — the same
three integers.

**REFUTED — §1's decode-order per-picture table.** My decode-order measurement
reproduces **9 of its 10** entries exactly — f8 80, f9 25, f10 5, f11 50, f12 3,
f14 46, f15 80, f16 67, f17 26 — but:

| decode picture | report §1 | my measurement |
|---|---|---|
| f13 | **80** | **15** |
| TOTAL | **317** | **397** (0 Y / 185 U / 212 V) |

The report's list sums to 462, its stated `TOTAL 317` is the display-order
figure, and 397 − 317 = 80 = the hidden picture f8. So §1's table is the
display-order total wearing a decode-order label, with one picture value wrong
by 65. The fix and its attribution are unaffected; the table is not.

**Confirmed — the gate is not tautological.** `a_lossless_444_altref_leaf8_warp_chroma_is_byte_exact`
lives at `stream.rs:8414`. Its comparator, `count_rawvideo_diffs`
(`stream.rs:10884`), runs `aomdec` as a **subprocess**, reads its `--rawvideo`
file off disk, decodes with `decode_stream`, packs with `pack_rawvideo`, asserts
the two lengths are equal (returning `None`, which the gate turns into a panic),
and zips. Both sides are genuinely read. Inside the gate there is an oracle-flip
arm (one byte of the oracle's own output rotated, count must move by exactly one
in U only) and a `compound_warp_hits_8` reachability witness. The comparator has
its own separate unconditional non-vacuity test,
`the_counting_oracle_diff_detects_one_flipped_oracle_byte`
(`stream.rs:10999`), which flips an oracle byte and requires exactly one wrong
sample and one fewer exact frame. This is the anti-`av1422anom` structure.

**Confirmed — the mutation number.** The report's mutation proof reverts the four
warp calls and the gate reds with `0 Y / 147 U / 170 V`. That is exactly what I
measure at the pre-fix state `3698787d`, independently, on the same basis.

**Confirmed — the source shape.** `59d3594a` adds four
`warp_affine_compound` calls (U ref0, U ref1, V ref0, V ref1) inside
`decode_inter_block8`'s compound chroma arms, each behind
`warp_plane_allowed(chroma_w, chroma_h)`, plus the tightened single-ref guard.

---

## 3. `2005a35d` lane-av1422llintra — **CONFIRMED**, every headline number exact

Measured on the report's own basis (display order, `dump_yuv` vs
`aomdec --rawvideo`), lane base `3698787d` → lane tip `a347784b`:

| cell | before | after |
|---|---|---|
| `ll422_allintra` | **Y 75 210 / U 38 235 / V 38 240** | **Y 0 / U 3 126 / V 3 139** |
| `W_intrabc` frame 0 | Y 75 632 / U 37 979 / V 38 070 | **0 / 0 / 0** |
| `X_intrabc_tiled` frame 0 | Y 59 775 / U 30 085 / V 30 738 | **0 / 0 / 0** |

"luma now byte-exact (75 210 wrong → 0)" — exact. "chroma 38 235 / 38 240 →
3 126 / 3 139" — exact. "frame 0 now byte-exact" for both W and X — exact.
Decode-order cross-check at the merge states: `ll422_allintra` is
`75210/38235/38240` at `3698787d`, `a20aba01` **and** `9367642a`, and
`0/3126/3139` at `2005a35d`, so llintra is the only commit in that span that
moves the cell.

**Minor non-reproduction.** The whole-cell totals the report gives for the
"both fixes" build: W `917974 / 474518 / 487693`, X `864325 / 421582 / 414905`.
I measure W `917974 / 474506 / 487692` and X `864325 / 421744 / 414838`. Luma
matches to the sample; chroma is off by 12 and 1 (W) and 162 and 67 (X). The
BEFORE totals in the same table — W `1214609 / 611596 / 612205`, X
`1072597 / 533606 / 539334` — match mine exactly. Direction and magnitude class
are right; three of the six "after" chroma figures do not reproduce.

**Confirmed — the source shape.** `a347784b` converts exactly **three**
`around_mi_rect` dc-sign gathers to `around_mi_422_chroma`, matching the report's
"3 chroma gathers"; `around_mi_422_chroma` has 53 call sites at main.

---

## 4. `5c216458` lane-av1422llpred — **CONFIRMED on the headline, one regression row REFUTED**

Isolated pair `2005a35d` → `29c21f51` (the merge `5c216458` also carries
`lane-av1oddheightfork2`'s instrumentation, so it is not the lane's own delta):

| cell | before | after |
|---|---|---|
| `ll422_allintra` | **U 3 126 / V 3 139** | **U 2 984 / V 3 067** — not exact |
| `ll420_allintra` | 0 / 0 / 0 | 0 / 0 / 0 |
| `ll444_allintra` | 0 / 0 / 0 | 0 / 0 / 0 |
| `ll444_c` | 18/18 exact | 18/18 exact |

Every headline quantity is exact, and the 4:2:0 / 4:4:4 controls are untouched.
The report is also honest that the cell is still not exact.

**REFUTED — the "Regression (W_intrabc / X_intrabc_tiled) — no change" row, on
the report's OWN display-order basis.**

| cell | plane | `2005a35d` | `29c21f51` | delta |
|---|---|---|---|---|
| `W_intrabc` | Y / U / V | 917974 / 474506 / 487692 | 917974 / 474506 / 487692 | **0 / 0 / 0** |
| `X_intrabc_tiled` | Y | 864325 | 864325 | 0 |
| `X_intrabc_tiled` | **U** | **421 744** | **421 746** | **+2** |
| `X_intrabc_tiled` | V | 414838 | 414838 | 0 |

`W_intrabc` is exactly delta 0, as claimed. `X_intrabc_tiled`'s **U plane moves
by 2 samples**, so the "no change" row is false as written. The report's own
absolute figures for X's chroma (421 718 / 414 871) also do not reproduce; I get
421 744 / 414 838, i.e. +26 and −33. The load-bearing conclusion — that
`X_intrabc_tiled` is not in this defect's class — survives; the stated quantity
does not.

**Confirmed — the source shape.** `29c21f51` changes 19 lines of
`crates/ec-av1/src/decode.rs`: `tu_reach` → `tu_reach_rect` with `4 << ss_y(fctx)`
at three sites (the found fix plus the two-site class sweep).

---

## 5. `ac00e9fb` lane-av1422llpred2 — **CONFIRMED**, including the delta-0 rows

Isolated pair `d202b8b4` → `7a603bef`. The merge `ac00e9fb` is **not** the lane
alone — `7a603bef..ac00e9fb` adds 67 lines to `ec-av1/src/stream.rs` and 158 to
`ec-av1/src/decode.rs` (lane-stackbox). Measuring the merge pair instead of the
lane pair produces a false refutation; I state this because a future audit will
hit it.

| cell | before | after |
|---|---|---|
| `ll422_allintra` | **0 / 2 984 / 3 067** | **0 / 0 / 0 — byte-exact** |
| `ll420_allintra` | 1/1 exact | 1/1 exact |
| `ll444_allintra` | 1/1 exact | 1/1 exact |
| `ll444_c` | 18/18 exact | 18/18 exact |

**Confirmed — the "delta 0" regression rows, on the report's own basis.**

| cell | `d202b8b4` | `7a603bef` | delta |
|---|---|---|---|
| `W_intrabc` Y / U / V | 917974 / 474506 / 487692 | 917974 / 474506 / 487692 | **0 / 0 / 0** |
| `X_intrabc_tiled` Y / U / V | 864325 / 421746 / 414838 | 864325 / 421746 / 414838 | **0 / 0 / 0** |

Six plane-totals, six zeros. This is the claim a merge-level measurement would
have refuted, and the isolated pair rescues it.

**Not reproduced — the absolute figures.** The report's before/after W totals
`807779 / 389623 / 418698` and X totals `761090 / 361159 / 386148` do not
reproduce on its stated base `d202b8b4`; I measure `917974 / 474506 / 487692`
and `864325 / 421746 / 414838`, a 110 195-sample difference on W. The delta-zero
property holds; the absolute numbers come from some other build.

**An omission, not an error.** `ll422_noibc` on the same pair moves
`Y 922750 / U 463030 / V 472221` → `U 452672 / V 462814` and goes 0/16 → 2/16
exact. The report's before/after table does not mention it.

**UNTESTED, with reason.** "Prediction-sum disagreements 453 → 0 of 9600" — I did
not re-run the `EC_PRED` / `EC_PREDOUT8` rung pairing. My measurement of
`ll422_allintra` is byte-exactness over all 153 600 bytes of the frame on all
three planes, which is strictly stronger: zero prediction-sum disagreements is a
consequence, not an extra fact. The rung ladder itself remains unverified.

---

## 6. `81a21c7d` lane-av1422llinter2 — **CONFIRMED**, counters and arm reachability reproduced

Lane base `cc211d13` (the merge's first parent) → `81a21c7d` (the merge), bypass
on both, decode order, all three planes:

| cell | before | after | claimed |
|---|---|---|---|
| `W_intrabc` | 1/17 exact, Y 986 852 U 508 140 V 523 735 | **17/17 exact, 0 / 0 / 0** | 1/17 → 17/17 ✓ |
| `X_intrabc_tiled` | 1/17 exact | **17/17 exact, 0 / 0 / 0** | 1/17 → 17/17 ✓ |
| `Y_intrabc_10b` | **4 pictures emitted of 17** | **17/17 exact, 0 / 0 / 0** | 1/4 → 17/17 ✓ |
| `ll422_noibc` | 2/16 exact | **16/16 exact, 0 / 0 / 0** | 2/16 → 16/16 ✓ |
| `ll420_allintra` | 1/1 exact | 1/1 exact | unchanged ✓ |
| `ll444_allintra` | 1/1 exact | 1/1 exact | unchanged ✓ |
| `ll444_c` | 18/18 exact | 18/18 exact | unchanged ✓ |

**Every gate-counter delta in the report reproduces EXACTLY** (`W_intrabc`):

| counter | report before → after | my measurement |
|---|---|---|
| `chroma422_rect` | 88 → **0** | **88 → 0** |
| `chroma422_sub8` | 169 → **391** | **169 → 391** |
| `chroma422_square` | 3 312 → 3 312 | **3 312 → 3 312** |
| `intra128_lossless` | 8 → 0 | **8 → 0** |
| `rect4_16_pair.lossless_chroma` | 106 → 72 | **106 → 72** |

**Arm reachability — measured, not assumed.** The fix changes a gate at **three**
call sites (single-ref inter, compound inter, intra-in-inter, all in
`decode_inter_block8`). A predicate fix proves nothing unless all three arms run,
so I added a temporary `thread_local!{Cell<usize>}` immediately after each of
the three gates and printed them from `examples/decode_probe.rs`, in the scratch
worktree, then discarded it with the worktree:

| cell | single-ref | compound | intra-in-inter |
|---|---|---|---|
| `W_intrabc` | 131 | 356 | **202** |
| `X_intrabc_tiled` | 148 | 376 | **178** |
| `Y_intrabc_10b` | 3 | 166 | **565** |
| `ll422_noibc` | 111 | 109 | 0 |
| `ll444_c` (4:4:4) | 423 | 771 | 13 |

**All three arms are exercised** on all three cells that went exact — the
intra-in-inter arm is the *most* exercised on `Y_intrabc_10b`. The pre-existing
`llintra8_chroma_walk` counter agrees exactly with my third column
(202 / 178 / 565 / 0 / 13). Note the trap I nearly fell into: that counter reads
**0 before** the fix, because the arm did not route there yet. Reading only the
"before" value would have produced a confident and completely wrong "this arm is
never reached".

**Class sweep — the conclusion holds, one description is wrong.** I enumerated
every `lossless` + chroma-shape gate in `decode.rs` at main. The only other
4:4:4-keyed lossless chroma gate in the tree is

```rust
} else if (chroma_444 || chroma_422) && lossless(fctx) {   // 24544
} else if chroma_444 && lossless(fctx) {                    // 24618
```

The report lists 24618 among sites that "were already shape-generic". It is not
shape-generic — it is **statically unreachable**: 24618's condition implies
24544's, so it can never be evaluated. "No fourth site exists" is therefore true
for every *reachable* site, and the report's characterisation of 24618 is wrong
in a way that would mislead a later reader into looking for a live 4:4:4-only
path there. The remaining lossless chroma gates (18365, 18366, 24500, 24752) are
shape-generic or the matching stamp guard.

**The full 4:2:2 corpus, all 34 cells, both states** (decode order, `lossy_all/`
plus `probe/`):

| cell | `cc211d13` | `81a21c7d` | report's §6 |
|---|---|---|---|
| `W_intrabc` | 1/17 | **17/17** | 1/17 → 17/17 ✓ |
| `X_intrabc_tiled` | 1/17 | **17/17** | 1/17 → 17/17 ✓ |
| `Y_intrabc_10b` | 1/17 (4 emitted) | **17/17** | 1/17 → 17/17 ✓ |
| `ll422_noibc` | 2/16 | **16/16** | 2/16 → 16/16 ✓ |
| `ll422_allintra` | 1/1 | 1/1 | ✓ |
| `422_allskip_2f` | 2/2 | 2/2 | ✓ |
| `422_intrabc_sb128_strip` | 5/5 | 5/5 | ✓ |
| `422_intrabc_sb128_strip_notxsearch` | 5/5 | 5/5 | ✓ |
| `422_residual_compound_warp_16f` | 16/16 | 16/16 | ✓ |
| `422_residual_compound_warp_nolr_16f` | 16/16 | 16/16 | ✓ |
| `422_sb128_3f` | 3/3 | 3/3 | ✓ |
| `AA_inter_compound` | 43/43 | 43/43 | ✓ |
| `AD_inter_nogm` | 43/43 | 43/43 | ✓ |
| `A_testsrc2_cpu0` | 17/17 | 17/17 | ✓ |
| `B_testsrc2_cpu6` | 17/17 | 17/17 | ✓ |
| `C_mandel320` | 16/16 | 16/16 | ✓ |
| `D_bars` | 16/16 | 16/16 | ✓ |
| `E_noglobal` | 17/17 | 17/17 | ✓ |
| `F_allintra` | 16/16 | 16/16 | ✓ |
| `H_10bit_testsrc2` | 17/17 | 17/17 | ✓ |
| `I_10bit_mandel` | 16/16 | 16/16 | ✓ |
| `J_10bit_lr0` | 17/17 | 17/17 | ✓ |
| `K_sct` | 17/17 | 17/17 | ✓ |
| `L_tiled` | 17/17 | 17/17 | ✓ |
| `R_odd322x240` | 17/17 | 17/17 | ✓ |
| `T_tilecols2` | 17/17 | 17/17 | ✓ |
| `U_tilerows1` | 17/17 | 17/17 | ✓ |
| `V_tile2x2_odd` | 17/17 | 17/17 | ✓ |
| `AB_inter_warp_odd` | 0/43 | 0/43 | 43 bad → 43 bad ✓ |
| `O_odd322x242` | 0/17 | 0/17 | 17 bad → 17 bad ✓ |
| `Q_odd320x242` | 0/17 | 0/17 | 17 bad → 17 bad ✓ |
| `S_odd326x242_10b` | 0/17 | 0/17 | 17 bad → 17 bad ✓ |
| `ll420_allintra` | 1/1 | 1/1 | ✓ |
| `ll444_allintra` | 1/1 | 1/1 | ✓ |

**34 of 34 rows reproduce, before and after.** The odd-height cluster is
byte-identical before and after and stays at 0 — the report's "untouched, as
instructed" is exact.

**Minor.** §6's summary line "24 of 31 unique 4:2:2 cells exact before". From the
report's own 31 rows, 8 are not exact before (W, X, Y, `ll422_noibc`, and the
four odd-height cells), so 23, not 24. "27 of 31 after" is correct. One-cell
arithmetic slip, same flavour as claim 2's total.

**Minor.** The §5 per-frame before-table reproduces 46 of 48 (W) and 38 of 48 (X)
per-plane per-frame numbers exactly; the Y column is exact on all 17 frames of
both cells. Two W entries drift (`f11` U 33 187 reported vs my 33 204; `f6` V
26 030 vs my 26 029) and ten X entries drift by ≤ 64 samples.

---

## 7. Summary

| merge | lane | verdict |
|---|---|---|
| `3698787d` | av1422lpf | **CONFIRMED** — 3 panics gone, 6 lossless controls byte-exact both states, cells honestly not-exact. Two sub-claims refuted (a non-existent Golomb refusal; a non-existent "does not decode to completion" blocker); §1.1's 4:2:0 counter-evidence not reproducible. |
| `9367642a` | av1444altref | **CONFIRMED** — 8/18 → 18/18 decode-order, 0/147/**170** = **317** display-order, gate reads both sides with an in-test flip control, mutation number reproduced. §1's decode-order table: f13 is 15 not 80 and the total is 397 not 317. |
| `2005a35d` | av1422llintra | **CONFIRMED** — 75 210 → 0 luma, 38 235/38 240 → 3 126/3 139, W and X frame 0 → 0/0/0, every headline integer exact. Three of six "after" whole-cell chroma figures do not reproduce. |
| `5c216458` | av1422llpred | **CONFIRMED on the headline** (U 3 126 → 2 984, V 3 139 → 3 067, still not exact). **REFUTED:** the "`X_intrabc_tiled` — no change" row; its U plane moves 421 744 → 421 746 (+2) on the report's own basis. |
| `ac00e9fb` | av1422llpred2 | **CONFIRMED** — `ll422_allintra` byte-exact, 4:2:0/4:4:4 controls unchanged, and both delta-0 regression rows hold exactly (six plane-totals, six zeros) on the isolated lane pair. Absolute W/X totals do not reproduce on its stated base. The 9600-unit prediction-sum ladder untested (subsumed by byte-exactness). |
| `81a21c7d` | av1422llinter2 | **CONFIRMED** — W 1/17 → 17/17, X 1/17 → 17/17, Y 4-of-17 → 17/17, `ll422_noibc` 2/16 → 16/16, all planes; **all five gate-counter deltas exact**; **all three fixed call sites measured as reached**; all 34 corpus rows exact. §1's description of `decode.rs:24618` as shape-generic is wrong (it is unreachable). |

No merge's byte-exactness claim is refuted. Five of the six are confirmed with
every headline integer reproduced on the lane's own basis. The defects I found
are confined to: two retracted symptoms in the lpf report, one mislabelled
decode-order total and one wrong per-picture value in the altref report, three
non-reproducing whole-cell chroma figures in the llintra report, one false
"no change" regression row in the llpred report, and one unreachable gate
described as live in the llinter2 report. None of them changes a fix's direction
or a gate's verdict.

## 8. What a future audit should not repeat

* Measure the lane's **branch parent → branch tip**, not the merge's
  first-parent → merge. `ac00e9fb` carries lane-stackbox's changes.
* State the basis with the number. Decode order and display order differ by the
  hidden alt-refs' contribution; on `ll444_c` that is 80 samples out of 397.
* A counter that reads 0 **before** a routing fix is evidence the arm was not
  reached *then*, not that it is never reached. Read it after.
* Iterate frames when computing per-plane totals over a multi-frame stream. I
  wrote that bug myself mid-audit: one frame's plane geometry applied to a
  16-frame concatenation reported `0/0/0` while a whole-file zip said 317.
* The oracle's `EC_CPRED` rung **segfaults** (exit 139, 0 bytes of stderr) on
  `W_intrabc.obu` with `EC_ZZCPRED=-1:-1:-1:-1`. Any future lane relying on that
  rung for 4:2:2 reachability will silently collect nothing.
