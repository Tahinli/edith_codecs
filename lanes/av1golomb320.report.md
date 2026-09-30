# lane-av1golomb320 — the mixed-lossless Golomb-tail refusal is DOWNSTREAM geometry, not a parser bug; and Can-3's fix REGRESSES a 640x480 cell

Base `main` = `32d944bf`. Worktree `/home/tahinli/.cache/wt/av1golomb320`, target dir
`$HOME/.cache/cargo-target-av1golomb320`, `EC_NOMEMGUARD=1`. Oracle
`~/.cache/aom-oracle/build/{aomdec,aomenc}` (shared, untouched).
**No source change authored in this lane.** Branch tip is `a49460c6` = `32d944bf` +
a clean cherry-pick of Can-3's `658d37fa` (needed to re-measure the refusal on the
fixed base). Every `decode.rs` line in that commit is Can-3's.

---

## 0. Verdict

Two findings, both measured, neither a fix of mine.

1. **The Golomb-tail refusal in this class is DOWNSTREAM of a block-GEOMETRY
   walk defect, not a distinct parser bug.** Paired `EC_SYMR` shows frame 0 is
   **bit-identical to the oracle for all 25 792 reads**, then we emit **40 extra
   reads at `mi=(32,32)`** the oracle never makes. The abort arrives **11 970
   reads later**. It cannot be the first symptom.
2. **Can-3's `658d37fa` removes the refusal on the cells I had, and INTRODUCES
   one on 640x480.** `mix_640x480_5.obu` (87 537 B) and `mix_640x480_6.obu`
   (115 615 B) decode fully at `8705034f` (== `32d944bf` for `decode.rs`, empty
   diff) and **REFUSE with `658d37fa` applied**. That is a supported 4:2:0 8-bit
   cell going decode → refusal: a red-before/green-after in the wrong direction.
   Reported to Can-3 over IRC; **not mine to fix** (its hunks are its own).

Also: **the 320x240 arm named in the brief does not refuse** on this tip — see §2.

---

## 1. The cells

Recipe (all arms): `testsrc2`, 6 frames, `--passes=1 --cpu-used=4 --end-usage=q
--cq-level=0 --aq-mode=1`. Per-size source: `ffmpeg -f lavfi -i
testsrc2=size=<S>:rate=25 -frames:v 6 -pix_fmt yuv420p`. ffmpeg 8.1.3, shared
oracle aomenc.

| cell | bytes | sha256 (full) |
|---|---:|---|
| `mix_176x144_6.obu` | 20 069 | `02b15d0d5b08aa5aebf139aee94c31b4f8363d14649c238cd7a19a9785ef124a` |
| `mix_176x144_5.obu` | 16 090 | `08fdbbb6566d4d51…` (prefix) |
| `mix_320x240_5.obu` | 28 716 | `40d613fd0ced7b90facdaf71f62501acad90fb789738851a2b59c92bd6a64149` |
| `mix_320x240_6.obu` | 38 686 | `37fbe7cb9235980d…` |
| `mix_352x288_6.obu` | 46 490 | `e43c74b8d847e701…` |
| `mix_640x480_5.obu` | 87 537 | `3d505fd2e0bbb7ca…` |
| `mix_640x480_6.obu` | 115 615 | `c82ba4d793d378cc…` |

On this content `--limit=8` produces the **same bytes** as `--limit=6` (sha
identical) for 176x144, 320x240, 352x288 and 640x480, so frame count is not a
variable; `--limit=6` is the minimal refusing arm where it refuses.

Exact refusal string, verbatim from `decode_probe`:

```text
REFUSED: unsupported: AV1 tile (a Golomb tail longer than this decoder reads)
```

## 2. The 320x240 premise is FALSE on this tip

At **both** `32d944bf` (main) and `8705034f` (the framecount report's base),
`mix_320x240_5.obu` decodes **6/6 decode-order dumps** with **zero** refusal
lines — it DIVERGES (f0 exact, f1..f5 differ, 90 813 / 85 821 / 99 314 / 90 572
/ 91 598 of 115 200 bytes). The framecount report's claim that the 320x240 arm
of this recipe refuses does not reproduce. Recorded, not chased.

## 3. The class sweep — refusal is content/size dependent

`--limit=6`, dual decode + per-frame `cmp`, `testsrc2`:

| size | bytes | at `8705034f` | at `658d37fa` (Can-3) |
|---|---|---|---|
| 176x144 | 20 069 | **REFUSED** (1/7 dumps) | decodes 7/7, f0 EXACT, f1..f6 red |
| 352x288 | 46 490 | **REFUSED** (3/7 dumps) | decodes 7/7, **7/7 EXACT** |
| 320x240 | 38 686 | decodes 7/7 (6 exact, 1 red) | decodes 7/7 (6 exact, 1 red) |
| 640x480 | 115 615 | decodes 7/7 | **REFUSED** (5/7 dumps) |

`--limit=5` on 640x480 (87 537 B) behaves the same: decoded at base, **REFUSED**
with `658d37fa`.

**So Can-3's 2-line change fixes 176x144 and 352x288 outright and breaks
640x480.** That asymmetry is the headline: the change is right about the
`!lossless` conjunct but incomplete, and there is at least one site where adding
`&& !lossless(fctx)` removes a read that libaom does make.

## 4. Lockstep measurement — the refusal is the SECOND symptom

Paired `EC_SYMR` (`EC_SYMR=1` both sides) on `mix_176x144_6.obu`. Compared on
`pre[0]`, `pre[1]`, `n`, `s`, `post_rng`; `pre[2]` (bit counter) carries the
known per-site constant offset and `cdf0` the ICDF-mirroring convention — neither
is the signal.

Frame boundaries = range-coder inits (`pre[1] == 32768`):

```text
ORACLE frame starts: 0, 25792, 41982, 55299, 70069, 83608, 85411   (7 frames)
OURS   frame starts: 0, 25832                                       (1 frame, then REFUSED)
```

**Frame 0 — bit-identical, then 40 extra reads:**

| reads | result |
|---|---|
| 0 … 25 791 | **identical** on all five compared fields |
| 25 792 … 25 831 | **ours only** — 40 extra reads, **all stamped `mi=(32,32)`** |

At read 25 792 the **oracle** does a range-coder init (`pre=(4399,32768,0)`) = the
frame ended; we are still reading. `mi=(32,32)` in 4×4-MI units is pixel
**(128,128)** — the **48×16 bottom-right sliver** of a 176×144 frame. Every extra
read is `tag=all_zero side=4 ctx=1`, i.e. a `txb_skip` **all_zero** flag on a
side-4 (chroma) transform, with the bit position **frozen** (`bit=36288`,
`post_bit=36288`, `range` decaying 509/read): these reads consume no data at all.
They are the signature of a walk that has run off the end of the tile and keeps
emitting units.

Frame 0's **pixels are byte-exact** despite the 40 extra reads — the defect is
the walk, not the reconstruction.

**Frame 1 — lockstep with the +40 offset, then the fork (at `8705034f`):**

```text
aligned oracle 25792+k  vs  ours 25832+k   for k = 0 … 3618   → IDENTICAL
oracle read 29411: decodetxb.c:158 mi=(9,14) pre=(41839,52168) n=2 s=0 post_rng=38760
ours   read 29451: ph=inter8    mi=(8,12) pre=(41839,52168) n=2 s=0 post_rng=35823
```

Same input `rng`, same `n=2`, same symbol `s=0`, different `post_rng` → different
CDF row for the same `txb_skip`. The preceding read is `decodemv.c:1036`
`cdf=mv_hp` on both sides.

**Distance from the fork to the abort: 11 970 reads.** The last symbol read
before the refusal is ours 41 381 at `mi=(24,40)`, `tell=24112`, frozen.

**On `658d37fa` the frame-1 fork moves from 29 411 to 41 913** (oracle
`mi=(28,40)` vs ours `mi=(32,0)`), and frame 0's 40 extra reads at `mi=(32,32)`
are **unchanged**. So Can-3's fix does not touch my site.

## 5. 640x480 under `658d37fa` — the regression, measured

`mix_640x480_5.obu`, paired `EC_SYMR` on Can-3's commit:

```text
ours 521 856 reads, oracle 457 696 reads
first divergence at read 179 378
  O 179378 pre=(24163,32784) n=2 s=0 post_rng=59748  mi=(85,100)
  U 179378 pre=(24163,32784) n=2 s=0 post_rng=43411  mi=(80,108)
```

Same `rng`, same `n`, same `s`, different `post_rng` — a geometry walk difference
at `mi=(85,100)` (pixel 340,400) vs our `mi=(80,108)`. Our read count **exceeds
the oracle's by 64 160**, so we decode ~64 k reads past where the oracle stops,
which is what eventually trips the Golomb cap. **Not diagnosed further**: the
hunks are Can-3's, the change is its own, and per the charter I report the
overlap rather than race it. Sent to Can-3 with the recipe, both sizes and the
fork read.

## 6. Where the Golomb tail length is computed vs consumed, and what libaom does

| | ours | libaom |
|---|---|---|
| **computed** | `read_golomb`, `crates/ec-av1/src/decode.rs:7989-8018` — counts the unary prefix with `dec.literal(1)`, `length += 1` per zero bit | `read_golomb`, `src/av1/decoder/decodetxb.c:22-43` — the identical loop |
| **cap** | `length > 20` → `Err(unsupported("a Golomb tail longer than this decoder reads"))` | `length > 20` → `aom_internal_error(xd->error_info, AOM_CODEC_CORRUPT_FRAME, "Invalid length in read_golomb")`, then `break` |
| **consumed** | `decode.rs:8469` (square TU) and `decode.rs:8747` (rect TU), each `if level > MAX_BR_LEVEL { level + read_golomb()? }` | `read_coeffs_txb` / `read_coeffs_txb_rect`, same file |

**Measured: the refusal fires in the SQUARE reader**, not the rect one.
`EC_TRACE_COEFF=1` ladder on `mix_176x144_6.obu`:

```text
EC_COEFF_STEP tag=base  c=2 pos=1 ctx=4 level=0 rng=62656
EC_COEFF_STEP tag=after_bases rng=57408
EC_COEFF_STEP tag=sign c=4 sign=0 rng=57464
EC_COEFF_STEP tag=post_golomb c=4 level=3 rng=57464     <- last successful golomb
EC_COEFF_STEP tag=sign c=6 sign=0 rng=57576
EC_COEFF_STEP tag=post_golomb c=6 level=2 rng=57576
EC_COEFF_STEP tag=sign c=7 sign=0 rng=57800
REFUSED: unsupported: AV1 tile (a Golomb tail longer than this decoder reads)
```

The block is `mi=(24,40)` (pixel 96,160, the right edge), `c=7` is a
high-frequency coefficient past `MAX_BR_LEVEL`, and the bit position is frozen at
`tell=24112`. **The tail is counted out of no data at all**, which is why it runs
past 20: `dec.literal(1)` returns 0 forever once the reader is exhausted.

**The cap is bit-identical to libaom's, and the tail is not a decoder limit
there.** The two loops are the same loop. The one real difference is what happens
on overflow: libaom's `aom_internal_error` (`aom/src/aom_codec.c:158-166`)
records the error and `longjmp`s to the caller's `setjmp` — that PICTURE is
abandoned but the decoder object survives; ours returns a hard `Err` that unwinds
`decode_stream` and takes the whole stream with it (1 dump instead of 7).
**Closing the read-side geometry removes the tail; widening or softening the cap
would only trade a clean refusal for silent corruption** — the existing comment
at `decode.rs:7990-8005` records exactly that from lane-scaledref r1 (commit
`ee1f980`, reverted in `314ee08`).

## 7. Relation to Can-3's fork

**Same class, different site.** Both are block-GEOMETRY divergences in the same
mixed-lossless + altref class and both land on a `txb_skip` read
(`decodetxb.c:158`) with the parse healthy up to that point. The 40 extra
all-zero reads at `mi=(32,32)` here are the mirror image of Can-3's missing
2-unit luma transform at `mi=(6,1)` on the 256x128 cell.

Can-3's fix does not move read 25 792 and does not change the `mi=(32,32)` walk
(§4), which is the direct evidence that the two sites are independent: his change
removes exactly one spurious `tx_size_cat1` read per lossless sub-8 leaf, which
would shift an index by a variable amount, not by the constant 40 measured here.
**A single fix closing both is NOT claimed.**

## 8. Hunk scope

* **Authored by me: zero lines of `decode.rs`.** The branch tip `a49460c6` is
  Can-3's `658d37fa` cherry-picked onto `32d944bf`, unmodified.
* Can-3's frozen hunks: `decode.rs:27140` and `decode.rs:48259` (both
  `&& !lossless(fctx)`), plus the `SUB8_LOSSLESS_NO_VARTX` counter. Not touched.
* The Golomb/tile read path was measurement-only throughout; no probe bypass was
  added, committed or left in the tree. Only env-gated rungs (`EC_SYMR`,
  `EC_AV1_TELL`, `EC_TRACE_COEFF`, `EC_ECDUMP_IN`, `EC_PROBE_HDR`,
  `EC_AV1_FINAL_DUMP`) were used, all read-only.
* Worktree `~/.cache/wt/av1golomb320`, private `CARGO_TARGET_DIR`, primary
  checkout never edited. `git status` in the primary checkout: clean.

## 9. Regression

Scoped 4:2:0 / 4:4:4 / 4:2:2 / lossless / stream families:

```
cargo test -p ec-av1 --lib -- 420 444 422 lossless stream \
  --skip bitrate_target_lands_within_5_percent_over_48_frames
```

Run on `a49460c6` (= main + Can-3's commit). Result in §11.

**Note the scope limit this run cannot cover:** the 640x480 regression in §5 is a
*decode* of a stream this lane generated on the fly; it is not in the fixture
corpus and no committed gate exercises it. A green §9 therefore does **not**
contradict §5, and §5 is the stronger evidence.

## 10. What is NOT measured

* **No fix, no gate, no fixture committed by this lane.** The reduction is handed
  over; nothing here may be described as gated.
* **The `mi=(32,32)` geometry owner is not identified.** Only that we emit 40
  all-zero chroma `txb_skip` reads the oracle does not, at the bottom-right
  sliver, consuming no data, with frame 0's pixels byte-exact regardless.
* **The 640x480 regression is measured but NOT root-caused.** Fork read 179 378
  and the mi pair are given; the owning code is not identified, and I did not
  chase it because it is inside Can-3's declared hunks.
* **Whether Can-3's fix closes the 176x144 fork entirely is NOT measured** — it
  removes the refusal but f1..f6 remain red (811 / 949 / 630 / 969 / 811 /
  24 105 of 115 200 bytes).
* **Only `testsrc2` content was swept** (4 sizes × 2 frame counts). No other
  generator, no 10-bit, no odd dimensions, no 4:4:4. The class may be wider.
* **No mutation proof** — there is no change of mine to mutate.
* The 320x240 and 176x144 pixel divergences are **not** diagnosed here beyond
  "same class as Can-3's, see §7".

## 11. Regression result

Run on `a49460c6` (= `32d944bf` + Can-3's `658d37fa`), `EC_NOMEMGUARD=1`, log at
`/tmp/g320/regress2.txt`:

```text
test result: ok. 374 passed; 0 failed; 7 ignored; 0 measured; 422 filtered out;
             finished in 1634.56s
```

Green — and **not** evidence against §5: the 640x480 cells are generated, not
committed fixtures, so nothing in this gate set decodes them. The sweep in §3 is
the only detector for that regression, which is why §3 is the load-bearing
evidence of this report and §9 is a no-delta confirmation.

## 12. Fix-now | deferred | accepted

* **fix-now:** nothing in this lane. The tree carries no change of mine.
* **deferred(unblock: Can-3 addressing §5):** the 640x480 cells are a REGRESSION,
  not a disposition — `mix_640x480_5.obu` must decode again. Owner: Can-3.
* **deferred(unblock: identification of the `mi=(32,32)` walk owner):** the 40
  extra reads and the residual f1..f6 on 176x144.
* **accepted:** the `length > 20` cap stays exactly as libaom has it (§6). The
  Golomb tail is the symptom; the read-side geometry is the cause.

## 13. Reproduction

```bash
cd /tmp/g320/sweep
ENC=~/.cache/aom-oracle/build/aomenc; DEC=~/.cache/aom-oracle/build/aomdec
P=$HOME/.cache/cargo-target-av1golomb320/debug/examples/decode_probe   # main + 658d37fa
PB=$HOME/.cache/cargo-target-av1golomb320-base/debug/examples/decode_probe  # 8705034f

# the refusal, at the pre-fix base
EC_NOMEMGUARD=1 $PB mix_176x144_6.obu 2>&1 | grep REFUSED
# REFUSED: unsupported: AV1 tile (a Golomb tail longer than this decoder reads)

# the regression: decoded at base, refused after Can-3's commit
EC_NOMEMGUARD=1 $PB mix_640x480_5.obu 2>&1 | grep -E 'OK:|REFUSED'   # OK: 5 frames decoded, 640x480
EC_NOMEMGUARD=1 $P  mix_640x480_5.obu 2>&1 | grep -E 'OK:|REFUSED'   # REFUSED: ... Golomb tail ...

# the 40 extra reads
EC_NOMEMGUARD=1 EC_SYMR=1 $P mix_176x144_6.obu 2>&1 >/dev/null | grep -c '^EC_SYMR'   # 105219
EC_NOMEMGUARD=1 EC_SYMR=1 $DEC --codec=av1 -o /dev/null mix_176x144_6.obu 2>&1 >/dev/null \
  | grep -c '^EC_SYMR'                                                                 # 106606
# reads 0..25791 identical; ours 25792..25831 are the extra mi=(32,32) all_zero reads
```