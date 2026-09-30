# lane-av1422llintra — the 4:2:2 lossless chroma context: the dc-sign vote was double-counted

Base `main` = `3698787d` (the merge of lane-av1422lpf). Worktree
`~/.cache/wt/av1422llintra`, branch `lane-av1422llintra`.

Two commits:

1. `1cd7c85f` — the merged gate is renamed to what it asserts and told what it
   does **not** cover. It was vacuous with respect to lane-av1422lpf's seven-site
   fix.
2. the decode change below — **UN-GATEABLE** (stated, with the reason, and NOT
   described as gated).

Nothing else is committed. `crates/ec-av1/src/stream.rs` carries **no** bypass
in the committed tree; the `EC_AV1_ALLOW_422_PROBE` patch was applied for the
measurements below and reverted (`git diff` on stream.rs after commit 1 is
empty).

---

## 0. Verdict

| | |
|---|---|
| first-divergent unit found | V 4x4 at chroma (24, 8), `BLOCK_32X8` at mi(2, 8) |
| its cause | the `dc_sign` CDF row, not the level |
| that cause | the above vote was summed **twice** at ss (1, 0) |
| sites fixed | 3 chroma gathers that still used the plain `around_mi_rect` |
| the pinned 4:2:2 all-intra lossless control | **luma now byte-exact** (74 162 wrong -> 0); chroma 38 235/38 240 -> 3 126/3 139 |
| `W_intrabc`, `X_intrabc_tiled` | frame 0 now **byte-exact**; total wrong samples down 24 % / 19 % |
| `Y_intrabc_10b` | **REGRESSION, stated plainly: it now REFUSES at frame 0** (`a Golomb tail longer than this decoder reads`) where it used to decode 16/16 with 1 210 281 wrong luma samples. See §6. |
| regressions in the 422/lossless/444 gate families | **0** — 70 passed, 0 failed |
| a gate for the decode change | **none, and none is possible** — §5 |

---

## 1. Goal A — the merged gate was lying, and is fixed

`the_pinned_422_lossless_inter_chroma_panics_refuse_by_name` asserted
`REFUSAL = "a chroma format of 4:2:2"` — the **sequence-header** refusal, which
fires before any of the seven per-axis extent sites is reachable, and which
fires identically on the pre-fix tree. Main built `aef4fa67` and measured the
identical string. The gate therefore passes before and after the fix: with
respect to the seven sites it is **vacuous**. Its only real content is the three
pins (size + `fnv1a64`).

It is now
`the_pinned_422_lossless_inter_witnesses_are_present_and_refuse_by_name`, its
doc says so, and it says the thing that is easy to get wrong: **no committed
test can reach a 4:2:2 decode at all**, because the bypass that would allow it
is a patch-run-restore hack that must never be committed — so the seven-site
panic fix's evidence is the bypassed manual measurement in
`lanes/av1422lpf.report.md`, not this gate.

The false claim is retracted in the doc rather than deleted silently: the gate's
old name said `..._panics_...`, and lane-av1422lpf's report claimed `W` and `X`
now REFUSE with `a Golomb tail longer than this decoder reads` at frame 2. That
refusal **does not reproduce** — with the bypass, all three cells decode 16/16
with no panic and no second refusal (measured, §2).

**A second gate from the same lane carried the same over-claim**, in the softer
form: `the_422_lossless_inter_chroma_walk_sites_stay_per_axis` says "one
assertion per site, each with a mutation proof", which reads like more coverage
than a source scan is. It is a **spelling** scan — it cannot observe a decode —
and its doc now says exactly that.

**Collateral damage found and repaired**: `69a3a4df` appended its own doc block
in the *middle* of the `lane-av1422warp` gate's doc comment, so
`the_pinned_422_residual_compound_warp_witness_is_present_and_refuses_by_name`
had lost its documentation entirely and two unrelated gates shared one comment
block. The warp doc is moved back onto its own gate.

The three pins are unchanged. Both gates pass:

```
$ cargo test -p ec-av1 --lib -- the_pinned_422_lossless_inter_witnesses_are_present_and_refuse_by_name \
      the_422_lossless_inter_chroma_walk_sites_stay_per_axis
test stream::tests::the_pinned_422_lossless_inter_witnesses_are_present_and_refuse_by_name ... ok
test stream::tests::the_422_lossless_inter_chroma_walk_sites_stay_per_axis ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 787 filtered out
```

---

## 2. The measurement rig, and why the numbers below are real

**Comparator** (`~/.cache/cells/av1422llintra/cmp422.py`, written here): pairs
our `dump_yuv` per-frame dumps against one flat `aomdec --rawvideo` file. The
per-frame plane geometry is derived from width/height/depth, never guessed; a
wrong SAMPLE is one `bps`-wide unit, so the 10-bit cell is counted per 16-bit
sample and not per byte.

**The comparator has a liveness guard, and it caught me.** A first version
globbed zero frame files (the 10-bit dump had refused) and reported
`Y 0 U 0 V 0 — first wrong: none`, which reads as BYTE-EXACT. That is the class
`oracle-comparator-liveness-control`: a comparator that reads nothing proves
nothing. It now hard-errors on an empty glob, and the "Y_intrabc_10b is exact"
line I would otherwise have written is **retracted** — see §6.

**Oracle-flip control, per cell** (the acceptance requirement): flip ONE byte of
the oracle dump and the count must move.

```
$ for c in W_intrabc:8 X_intrabc_tiled:8 Y_intrabc_10b:10; do cell=${c%%:*}; d=${c##*:}; \
    cp $cell.oracle.yuv /tmp/flip.yuv; \
    python3 -c "b=bytearray(open('/tmp/flip.yuv','rb').read()); b[3]^=0x01; open('/tmp/flip.yuv','wb').write(bytes(b))"; \
    ./cmp422.py $cell.<build> /tmp/flip.yuv 320 240 $d ""; done
```

| cell | build | count | count with oracle byte 3 flipped |
|---|---|---|---|
| W_intrabc | after | Y 1 220 912 | Y 1 220 913 |
| X_intrabc_tiled | after | Y 990 733 | Y 990 734 |
| Y_intrabc_10b | after | Y 1 210 281 | Y 1 210 282 |
| W_intrabc | both fixes | Y 917 974 | Y 917 975 |
| X_intrabc_tiled | both fixes | Y 864 325 | Y 864 326 |
| ll422_allintra | both fixes | Y 0 / U 3 126 / V 3 139 | Y 1 / U 3 126 / V 3 139 |

Every control moves by exactly the one flipped sample. The comparator is live.

**Oracle soundness**: the oracle dumps are `~/.cache/aom-oracle/build/aomdec`
(the instrumented tree). On `ll422_allintra.obu` its raw output is
sha256 `ae45d5dc220527b29f6a4b4d69583639f1ca3e70b4e284374ffde62ee6ebb84e`,
which is the value the previous lane measured for the instrumented build, the
plain build, the second build and `ffmpeg -pix_fmt yuv422p`. The oracle is
trustworthy for this stream.

`W_intrabc.oracle.yuv` and `X_intrabc_tiled.oracle.yuv` are byte-identical to
each other (`ea259d96fd2a2ff7...`) — the tiled re-encode of the same source
decodes to the same picture, so their divergence counts are directly comparable.

**Oracle dumps produced here:**

```
$ ~/.cache/cells/av1422llintra/oracle_run.sh <cell> <stream.obu> <depth>
W_intrabc.oracle.yuv        2 457 600 B  ea259d96fd2a2ff7...
X_intrabc_tiled.oracle.yuv  2 457 600 B  ea259d96fd2a2ff7...   (same bytes)
Y_intrabc_10b.oracle.yuv    4 915 200 B  dceee9c2ff925845...
ll422_allintra.oracle.yuv     153 600 B  ae45d5dc220527b2...
```

---

## 3. Per-cell before / after (raw commands)

Three builds of the same worktree, all with the same `EC_AV1_ALLOW_422_PROBE`
bypass, all from `dump_yuv`:

* **BEFORE** = `main` 3698787d exactly (`git checkout -- crates/ec-av1/src/decode.rs`)
* **after** = BEFORE + Merve2-2's `decode_leaf8` chroma-unit mechanism
* **both fixes** = after + the dc-sign gather fix (this lane)
* **dc-only** = main + the dc-sign gather fix WITHOUT Merve2-2's mechanism
  (the isolation control, §6)

```
$ export CARGO_TARGET_DIR=<per-build> EC_NOMEMGUARD=1 EC_AV1_ALLOW_422_PROBE=1
$ cargo build --release --example dump_yuv -p ec-av1
$ for c in W_intrabc X_intrabc_tiled Y_intrabc_10b; do \
    ./dumpyuv_<build> ~/.cache/cells/av1422lpf/$c.obu <cells>/$c.<build>; done
$ ./dumpyuv_<build> ~/.cache/cells/av1422lpf/probe/ll422_allintra.obu <cells>/ll422.<build>
$ ./cmp422.py <cells>/$c.<build> <cells>/$c.oracle.yuv 320 240 <8|10> "$c <build>"
```

320x240, 4:2:2, 16 shown frames (1 for `ll422_allintra`).

| cell | build | Y wrong | U wrong | V wrong | first wrong |
|---|---|---|---|---|---|
| `W_intrabc` | BEFORE | 1 214 609 | 611 596 | 612 205 | frame 0 Y sample 54 |
| | after (Merve) | 1 220 912 | 612 668 | 611 842 | frame 0 Y sample 54 |
| | **both fixes** | **917 974** | **474 518** | **487 693** | **frame 1 Y sample 0** |
| `X_intrabc_tiled` | BEFORE | 1 072 597 | 533 606 | 539 334 | frame 0 Y sample 32 |
| | after (Merve) | 990 733 | 488 493 | 490 484 | frame 0 Y sample 32 |
| | **both fixes** | **864 325** | **421 582** | **414 905** | **frame 1 Y sample 0** |
| `Y_intrabc_10b` (10-bit) | BEFORE | 1 227 876 | 613 885 | 614 058 | frame 0 Y sample 8 (= byte 16) |
| | after (Merve) | 1 210 281 | 612 170 | 613 127 | frame 0 Y sample 48 (= byte 96) |
| | **both fixes** | **REFUSES at frame 0** — §6 | | | |
| `ll422_allintra` (1 frame, pure intra) | BEFORE | 75 210 | 38 235 | 38 240 | frame 0 Y sample 16 |
| | after (Merve) | 74 162 | 36 703 | 37 114 | frame 0 Y sample 64 |
| | **both fixes** | **0** | **3 126** | **3 139** | frame 0 **U** sample 1853 |

What this says, plainly: **Merve2-2's chroma-unit mechanism alone is not a win**
(`W` gets worse, `X` and `ll422` better, none exact). With the dc-sign gather fix
on top, `W`/`X` lose another 24 %/19 % of their wrong samples and — the
measurement that matters — **frame 0 of both becomes byte-exact**, and the pure
intra control's **luma becomes byte-exact**.

---

## 4. The first divergence, localised

### 4.1 The pairing

Three paired rungs, all sequential, all env-gated:

* `EC_PRED` (ours, `OUR_PRED`) vs the oracle's `EC_PREDOUT8` — prediction, per TU
* `EC_TRACE_COEFF` (both sides, `EC_COEFF_STEP`) — the entropy symbol stream
* `EC_DBGCTX` (oracle) vs the crate's own `EC_CHROMA_PUB` / `EC_DCDUMP` — the
  per-cell entropy-context bytes that feed the dc-sign vote

Four rung-shape differences are normalised away, each because it is a difference
in what is PRINTED, not in what is READ (all four are named in the header of
`pair_coeff.py`): `tx_type` (a lossless block reads no tx_type symbol, so the
oracle's rung fires once per block where ours does not), `ctx`/`dcctx` (printed on
one side only), `c`/`pos` (compared only when both print them), and our
`tag=br` run inside the base_eob arm (libaom prints one finished level where we
print the whole base-range loop).

### 4.2 Before the dc fix: unit 189 of 9 600

```
FIRST DISAGREEMENT at unit index 189
  oracle: mi=(2,8) plane=2 row_off=1 col_off=1 txw=4 txh=4 mode=8 sum=2408
  ours  : x=20 y=12 plane=2 bw=4 bh=4 mode=8 sum=2410
```

and the entropy stream, 4 292 symbols in:

```
   [4291] O: EC_COEFF_STEP tag=after_bases rng=40604
        U: EC_COEFF_STEP tag=after_bases rng=40604
   [4292] O: EC_COEFF_STEP tag=sign c=0 sign=1 dcctx=0 rng=51524      <== MISMATCH
        U: EC_COEFF_STEP tag=sign c=0 sign=0 rng=37124
```

`rng` agrees to the bit (40 604 on both sides), so this is a **CDF row**
difference, not a bit-position difference — and the level read on both sides is
2. The oracle's `dc_sign` vote for that unit is `0`, ours is positive.

`EC_COEFF plane=2 row=0 col=2 mi_row=2 mi_col=8` names the unit: the third 4x4
V unit of the `BLOCK_32X8` at mi(2,8), i.e. **chroma (24, 8)**.

### 4.3 The oracle's own vote, and ours

libaom `get_txb_ctx_general` (`txb_common.h:280`) sums one `signs[]` vote per
CHROMA cell over `txb_w_unit` above entries and `txb_h_unit` left entries — for
a TX_4X4 chroma unit, **one** above entry and **one** left entry — and indexes
`dc_sign_contexts[dc + 32]`.

```
$ EC_DBGCTX=1 aomdec ... ll422_allintra.obu
EC_DBGCTX mi=2,8 plane=2 tx=0 above0=23 left0=13 base=2 off=10 ctx=12
EC_DBGCTX mi=2,8 plane=2 tx=0 above0=23 left0=12 base=2 off=10 ctx=12
```

Byte 23 = `16 | 7` (positive DC, level 7); byte 13 = `8 | 5` (negative DC, level
5). `signs[23>>3] = +1`, `signs[13>>3] = -1`, sum 0, so
`dc_sign_contexts[0 + 32] = 0` — `dc_sign_cdf[0]`, which is the row the oracle
read (`dcctx=0`).

Ours (`EC_DCDUMP=1`, the crate's own per-cell gather):

```
EC_DCDUMP mi=(2,12) plane=2 wh=(8,4) vote=1 above=[Some(false)/7,Some(false)/7] left=[Some(true)/5]
```

**Two above cells.** At ss (1, 0) one chroma column spans TWO luma mi columns, so
the plain `around_mi_rect` walk summed the column's whole-unit dc sign twice:
vote `(+1) + (+1) + (-1) = +1` where libaom's is `(+1) + (-1) = 0`.
`dc_sign_ctx` only sees the vote's SIGNUM, so the doubling is invisible except
exactly when above and left cancel — `a + l = 0` (ctx 0) becomes `2a + l = +a`
(ctx 2). That is the whole defect, and it is why 4:2:0 and 4:4:4 are immune
(there the plane block is square, so the doubling scales the vote uniformly and
the signum is preserved).

### 4.4 The fix

Three chroma gathers still called the plain `around_mi_rect` where twenty-odd
sibling sites already route ss (1, 0) through `around_mi_422_chroma` (which
samples every second above cell — libaom's per-chroma-cell count):

* `decode.rs:12735` — `decode_rect_split`'s lossless chroma walk (**the site that
  produced the measured fork**)
* `decode.rs:17802` — `read_intrabc_rect_chroma_split`
* `decode.rs:19922` — `decode_block_rect64`'s tiled chroma walk

Each is now `if ss_x == 1 && ss_y == 0 { around_mi_422_chroma(..) } else {
around_mi_rect(..) }` — byte-identical to the existing sites at 12865, 13069,
14327 and 44920, and a no-op at 4:2:0 and 4:4:4 by construction.

### 4.5 After the fix

```
$ ./pair_coeff.py oracle_COEFF.log ours_COEFF_FIX.log
oracle symbols 311484  ours 311484
no disagreement in the first 311484 paired symbols
```

**The entire coefficient-symbol stream of `ll422_allintra.obu` now pairs symbol
for symbol**, and the prediction trace pairs for 1 726 consecutive units (it was
189). Luma is byte-exact. What is left is 6 265 chroma samples, and it is
localised:

```
$ ./firstdiff.py ll422.FIX ll422_allintra.oracle.yuv 320 240 8
frame 0 U (93,11) sample 1853: ours 159 oracle 160
```

* **first mis-reconstructed unit: the U 4x4 chroma unit at chroma (92, 8)**, in
  the 8x8 leaf at **mi(2,46)** (`EC_IMODE mi_row=2 mi_col=46 bsize=3` =
  `BLOCK_8X8`, `mode=2 uv_mode=2`).
* it is a **prediction** difference, not a residual one: our
  `OUR_PRED sum=2539` against the oracle's `sum=2542`, with `row0` and `col0`
  byte-identical on both sides — so the difference is interior-only, while both
  edges in the plane are byte-identical to the oracle (the units above, at
  (92,4), and to the left, at (88,8), are not in the differing set).
* the oracle's filters are **inert** on this frame, so this is reconstruction,
  not filtering: measured by dumping the oracle's own pre-loop-filter plane
  (`EC_AV1_PREFILT_DUMP`) and comparing it to its final output —
  `oracle prefilter-vs-final chroma differing samples: 0`, luma `0`.
* 496 of 4 800 chroma 4x4 units differ, 6 265 samples; 302 of those units differ
  in all 16 samples. Only V and U; luma is exact.

**Handed over, named:** the 4:2:2 leaf-8 chroma **prediction** for a
`BLOCK_8X8` chroma unit whose interior differs while both of its edge rows and
its edge column match the oracle byte for byte. The entropy stream is provably
clean up to and past it, so the defect is inside the prediction for this
mode/shape, not in its context gather and not in its residual.

---

## 5. Why there is no gate for the decode change, and what was done instead

**It cannot be gated, and this lane does not pretend otherwise.** 4:2:2 is refused
at the sequence header by name (`stream.rs:1803`), and the bypass that would
allow a decode is a patch-run-restore hack that must never be committed — so no
committed test, in any build, can execute the arms this change touches. There is
no source-scan substitute that would mean anything here: scanning for the guard's
spelling proves the spelling, which is precisely the criticism §1 levels at the
merged gate.

What was done instead, and what it is worth:

1. **The measurement is the evidence**, with the comparator's own liveness
   control per cell (§2) and an isolation control that separates this fix from
   the previous lane's (§6).
2. **The revert moves the numbers**, which is the only mutation proof available
   for unreachable code. Reverting the dc gather (that IS the `BEFORE` /
   `after` / `dc-only` row set) puts `ll422_allintra`'s luma back from 0 to
   75 210 wrong samples and its first divergence back to frame 0 Y sample 16,
   and puts `W`/`X` back from frame-0-exact to frame-0-diverging. The change is
   load-bearing; measured, not asserted.
3. **The three sites are swept as a class**, not one instance: every remaining
   `around_mi_rect` chroma gather in `decode.rs` was classified, and the three
   that can see ss (1, 0) are fixed. The other `around_mi_rect` call sites are
   LUMA gathers (12449, 12499, 12553, 18661, 18712, 18770, 23290, 23383) or are
   already guarded, and `around_mi` (18186, 19039) is a one-cell gather with no
   above extent to double.

This is stated as an **un-gateable correctness fix whose evidence is the
measurement**, not as a gated change.

---

## 6. The one regression, stated plainly

**`Y_intrabc_10b` now refuses at frame 0.** With both fixes it returns
`unsupported: AV1 tile (a Golomb tail longer than this decoder reads)`
(`frames_dispatched: 0`). Before, it decoded 16/16 with 1 227 876 wrong luma
samples.

Isolation control (the same three sites patched onto **main**, without the
previous lane's chroma-unit mechanism):

| build | W | X | Y_intrabc_10b | ll422_allintra (luma) |
|---|---|---|---|---|
| main (BEFORE) | 1 214 609 | 1 072 597 | 1 227 876 (decodes) | 75 210 |
| + Merve's mechanism only | 1 220 912 | 990 733 | 1 210 281 (decodes) | 74 162 |
| + dc gather only | 1 218 768 | 1 072 310 | **REFUSES at frame 0** | 75 981 |
| + both (**this lane**) | 917 974 | 864 325 | **REFUSES at frame 0** | **0** |

So the two defects **interact**: the dc gather alone is neutral-to-slightly-worse
(it moves `ll422`'s fork *earlier*, to Y sample 16, because the leaf-8 defect
then trips a different symbol first), and only the pair is a large win. The
`Y_intrabc_10b` refusal is caused by the dc gather alone, not by the
interaction.

Reading: on that stream the residual 4:2:2 defect now reaches a long unary run
in frame 0 and trips the named Golomb guard, where before it produced wrong
pixels instead. That is the crate's own honesty convention (a named refusal
beats silently wrong pixels) and it matches how lane-av1422lpf reported its own
panic-to-refusal conversion — but it IS a behaviour change on a cell, it is not
covered by any committed test, and **it is Main's call whether to keep it**. The
case for keeping it: the alternative leaves a defect diagnosed to the line and
the CDF row, unfixed, with `W`/`X` 19–24 % worse and the intra control's luma
wrong in 74 162 samples.

`Y_intrabc_10b`'s own divergence was not localised past this: its 10-bit trace
pairing is not trustworthy with the four rung-shape normalisations above (the
oracle emits its eob `br` symbols before the `base_eob` line there, the reverse
of the 8-bit cell), so I stopped rather than report a fork index I cannot
defend. **Not claimed.**

---

## 7. Refuted hypotheses, with the evidence

* **"the previous lane's report's numbers are wrong"** — partly. Its `W`/`X`/
  `Y_intrabc_10b` per-plane counts do not reproduce under a per-SAMPLE
  comparator (mine, §3). Its luma counts on `ll422_allintra` (75 210) reproduce
  exactly. The difference is the plane attribution: counting 10-bit chroma
  BYTE-wise counts the always-zero top 6 bits of each sample, which cannot
  differ, so U and V coincide in ways a byte count hides.
* **"the `Golomb tail` refusal on `W`/`X` at frame 2 exists"** — **refuted**.
  With the bypass, all three cells decode 16/16 on this tip with no panic and no
  second refusal. The claim is retracted in the gate's doc (§1).
* **"the residual is a transform / dequant defect"** — **refuted as the first
  cause** on the intra control: after the dc fix the whole 311 484-symbol
  coefficient stream pairs, every level matches, and luma is byte-exact. A
  dequant defect cannot leave the symbol stream and luma clean.
* **"the residual is a filter (deblock / CDEF / restoration) defect"** —
  **refuted**: the oracle's own pre-loop-filter plane equals its final output on
  every plane of this frame (0 differing samples), so the filters are inert here
  and cannot be where our chroma diverges.
* **"the `txb_skip` / `all_zero` `ctx` mismatch at unit 3 is the fork"** —
  **refuted** (re-confirmed): it is a label-space artifact — our chroma tables
  hold the offset-7 rows first, so our `ctx=0` is libaom's `ctx=7` — and the
  traces agree once `all_zero`'s `ctx` field is excluded.
* **"the oracle is unsound"** — **refuted**: §2, sha256 `ae45d5dc220527b2...`,
  matching the previous lane's four-way agreement.
* **"my comparator proved `Y_intrabc_10b` byte-exact"** — **refuted by the
  comparator's own liveness guard** (§2). I am recording the retraction because
  the false line was produced by a real command, not by a typo.

---

## 8. Regression sweep

```
$ CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1422llintra EC_NOMEMGUARD=1 \
  cargo test -p ec-av1 --lib -- 422 lossless 444 \
    --skip bitrate_target_lands_within_5_percent_over_48_frames
test result: ok. 70 passed; 0 failed; 0 ignored; 0 measured; 719 filtered out; finished in 26.30s
```

**70 passed, 0 failed.** That includes the whole 4:4:4 family, the lossless
transform family, and every pinned 4:2:2 refusal-by-name gate. The skip is the
known separate in-process hang.

`cargo check -p ec-av1 --all-targets` is clean.

---

## 9. Fixture provenance

| file | bytes | sha256 |
|---|---|---|
| `W_intrabc.obu` | 130 320 | `0aad0d6fdd23655749110baf7db6e831d78103ef4bd0355dcd49ba6276af310c` |
| `X_intrabc_tiled.obu` | 131 696 | `e7c0c60af1a615312196bfba042b558aca73302ce6c7b85d93096369192fb40b` |
| `Y_intrabc_10b.obu` | 202 281 | `ce84d7cb4cfadf3f52f783628dfbfcf5c07059ddbc356889967f4e7a764b2c22` |
| `probe/ll422_allintra.obu` | 43 255 | `d56f6b655743897ad998f7dbba7742cb4c3fa677b544d72fddb2d8223085dfb8` |

No instrumentation remains in the committed tree. Every trace used above is the
crate's own env-gated rung or the oracle's pre-existing one; nothing was left
behind.

## 10. `not_done`

- **`Y_intrabc_10b` is not localised** past "it refuses at frame 0" (§6), and its
  fork is not reported because its 10-bit rung-shape normalisation is not
  trustworthy.
- **The remaining `ll422_allintra` chroma residual is handed over, not fixed**
  (§4.5): the leaf-8 chroma PREDICTION at chroma (92, 8), mi(2,46), interior-only.
- **`W`/`X` frames 1..15 are not localised** — only frame 0 is now exact.
- **No committed gate exists for the decode change and none can** (§5).
- **No full-suite run** (lane rules: scoped only; project-wide validation is
  Main's).
