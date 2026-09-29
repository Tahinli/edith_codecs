# lane-ibc444c — the 4:4:4 intra-BC chroma fork

Branch `lane-ibc444c`, fresh off `a21f3680` (not a continuation of
`lane-whtshape` — that branch's lossless WHT work is separate and stays
there). One commit `6bff63b4`.

Files: `crates/ec-av1/src/decode.rs`, `crates/ec-av1/src/stream.rs` (the new
gate). Witness already committed, nothing pinned:
`crates/ec-av1/fixtures/444_intrabc_rect4_witness.obu`, 1016 B, sha256
`7fe888edf18caa74bdb415f0de643dc59963e87e0255d77191200b7a62eb2542`, FNV
`0x27c2_fad5_4047_2994`; 4:2:0 twin `420_intrabc_rect4_witness.obu`, 890 B,
FNV `0x57ec_3da2_a63b_403a`.

---

## r1 — Caller and plane: ESTABLISHED (this was the deliverable's first half)

Not by reading. This commit adds the **`EC_IBCTX`** rung to `read_coeffs_rect`
and the **`EC_IBCTX2`** rung to `read_coeffs`, both env-gated, both printing
`w, h, skip_ctx, sign_ctx, entry/post stream bit` and the `#[track_caller]`
site — and **no plane label at all**.

Two pairing hazards, both hit and both worth recording:

1. **The existing `EC_COEFF_STEP` rung hardcodes `plane=0`** (`decode.rs:7845`
   and the square reader). Pairing on it attributes every chroma unit to luma.
   The new rungs deliberately carry no plane; the plane comes from the
   pairing.
2. **The 16-bit `rng` is not a usable pairing key at this scale.** With 2395
   units, `difflib` on the `rng` ladder produced **2519 non-equal blocks** and
   a "largest common run" of 80 that landed at a completely unrelated mi —
   pure birthday collisions. The exact key is the **stream bit position**:
   our `SymbolDecoder::debug_bitpos()` against libaom's `aom_reader_tell()`
   (the oracle's `EC_ECDUMP_IN` third field), with a **constant +15 bit
   offset** = the OBU header libaom counts before `aom_reader_init`. With
   that key the ladders agree to the unit.

The fork, verbatim:

```text
oracle  bit 6870  plane=0 mi=(92,104) tx=7 (TX_8X16) all_zero=0
ours    bit 6885  8x16 skip_ctx=0 sign_ctx=0  decode.rs:28451 (read_inter_plane_rect)
oracle  bit 6878  plane=1 mi=(92,104) tx=7 (TX_8X16) all_zero=1
ours    bit 6893  4x8  skip_ctx=0 sign_ctx=0  decode.rs:28451            <-- FORK
oracle  bit 6879  plane=2 mi=(92,104) tx=7 (TX_8X16) all_zero=1
ours    bit 6902  4x8  skip_ctx=0 sign_ctx=0  decode.rs:28451
```

(`TX_SIZE` order from `aom_dsp/txfm_common.h:25`: 0 TX_4X4, 1 TX_8X8,
2 TX_16X16, 3 TX_32X32, 4 TX_64X64, 5 TX_4X8, 6 TX_8X4, **7 TX_8X16**,
8 TX_16X8, 9 TX_16X32, 10 TX_32X16, 11 TX_32X64, 12 TX_64X32, 13 TX_4X16,
**14 TX_16X4**, 15 TX_8X32, 16 TX_32X8, 17 TX_16X64, 18 TX_64X16.)

So, answering the question as posed:

* **Plane: 1 (U).** The first *symbol-value* disagreement is the U plane's
  `txb_skip` at mi (92, 104) = px (368, 416): the oracle reads `all_zero = 1`
  (unit skipped), we read `all_zero = 0`. Note the ticket had this inverted
  ("the ORACLE reads a chroma txb_skip"); what is true is that the two sides
  disagree on the *value* of that `txb_skip`, and the oracle's is 1.
* **Caller: `decode_intrabc_rect` → `read_inter_plane_rect` →
  `read_coeffs_rect(w = 4, h = 8, skip_ctx = 0)`.** The `w = 4, h = 8` in the
  ticket was right; the line number had drifted to `28451` in
  `read_inter_plane_rect`'s own `read_coeffs_rect` call.
* **Root cause: the unit SHAPE, and it comes from the FOOTPRINT.**
  `read_inter_plane_rect` is handed `(cw, ch)` by `decode_intrabc_rect`, which
  computed it as `bw / 2, bh / 2`. The luma block is 8x16, so the halving
  produced a **4x8** chroma unit where the chroma plane block is really 8x16
  and libaom codes it as **one TX_8X16**.

**Not a token-order defect, confirmed independently.** The earlier rounds'
"2688 vs 2395, no blowup" reading was right and the raw counts mislead: our
trace emits 3999 `all_zero` entries but **2673 of them consume zero bits**
(the square reader logs a step even for a 0-bit read), so the real symbol
count matches.

## r2 — The libaom sites that decide the shape, and the fix

Two sites, neither of which halves:

* `av1_get_max_uv_txsize` (`av1/common/blockd.h`) is `ss_size_lookup[bsize]`
  — the plane block is `bw >> ss_x` by `bh >> ss_y` at
  `(px >> ss_x, py >> ss_y)`.
* `get_vartx_max_txsize` (`blockd.h:1447`) reads that plane block as
  `max_txsize_rect_lookup[plane_bsize]` (luma) or
  `av1_get_adjusted_tx_size` of it (chroma), and
  `decode_token_recon_block` (`decodeframe.c:972-1005`) walks the units from
  that.

The fix is the two lines in `decode_intrabc_rect`:

```rust
let (cpx, cpy) = (px >> ss_x(fctx), py >> ss_y(fctx));
let (cw, ch) = (bw >> ss_x(fctx), bh >> ss_y(fctx));
```

replacing the hardcoded `/ 2`. **At 4:2:0 this is the same arithmetic**
(`ss_x = ss_y = 1`), which is why the 4:2:0 twin is a first-class arm of the
gate and not a claim.

This is the *same defect class* the `lane-whtshape` commit fixed on the
**lossless** arm of this very function; this lane does it unscoped, so the
lossy 4:4:4 cell falls out with it.

## r3 — Measurements

**Entropy (the strong claim).** After the fix, our `all_zero` entry bit
matches the oracle's for **all 2359 oracle units that report a real bit
position**, with every `txb_skip` value agreeing. The last 36 oracle units
report an `aom_reader_tell` sentinel near 2^32 — an oracle-side artifact
(`8 * (bptr - buf) - (cnt + 15)` wrapping at the end of the buffer), not a
fork. The witness's coefficients are now **identical to the oracle's**, so the
remaining pixel work is reconstruction, not bitstream.

**Pixels**, `444_intrabc_rect4_witness.obu`, 640x480 4:4:4 8-bit, frame 0,
against the oracle's `EC_AV1_FINAL_DUMP`:

| | Y | U | V | total |
| --- | --- | --- | --- | --- |
| before | 195615 | 149314 | 45051 | **390380** of 921600 |
| after | **0** | 72945 | 64865 | **137810** of 921600 |

Luma is now byte-exact on all 307200 samples. U and V are byte-exact for
their first 51360 samples — the whole frame above row 80, plus row 80 out to
column 160.

**OPEN after this lane (measured):** 72945 U + 64865 V samples still differ,
first at **(x=160, y=80)** on both planes. Because the coefficient ladder is
symbol-exact, this is a **reconstruction** divergence — the residual is being
read at the right place with the right values and written to the wrong place,
or a neighbouring unit's context is being clobbered. Candidates left standing,
in the order I would test them:

1. `decode_intrabc_owned_rect` (`decode.rs:16221`) and `decode.rs:19294` still
   carry the same `let (cw, ch) = (bw / 2, bh / 2);`. The post-fix trace shows
   no remaining halved chroma footprint on THIS witness, so neither is reached
   here — but both are the same latent defect on the same class of stream and
   should be swept by whoever owns them.
2. The per-unit chroma entropy-context replay: `decode_intrabc_rect`'s tail
   `record_split_luma_rect_mi` rewrites every chroma cell with ONE composed
   state, which drops the per-unit DC signs and levels (class
   `override-slot-on-one-arm`). `lane-whtshape` added exactly that replay to
   this function's lossless arm; the LOSSY arm has no such replay, and at
   4:4:4 the chroma plane block is now 2x4 units instead of one, so the
   composed state is wrong for a region the old single-unit code got right by
   accident. **This is my leading candidate for (160, 80)** and it is the
   first thing the next lane should test.
3. Nothing in the remaining 4x8 / 8x4 chroma rect reads is a halved 4:4:4
   footprint: the post-fix trace attributes them to `decode_rect4_16_strip`
   (1:4 strips, where an 8x4 chroma unit IS what
   `av1_get_adjusted_tx_size(TX_16X4)` gives) and to the
   `read_chroma_coeffs_rect` wrapper.

## r4 — Gate and mutation proof

`stream::tests::a_444_intrabc_rect_chroma_plane_block_is_the_block_footprint`:

1. 4:4:4 8-bit header asserted (`assert_444_header`) — the defect needs
   `ss_size_lookup` to differ from the 4:2:0 halving;
2. `IBC_RECT_CHROMA_FOOTPRINT_444_HITS > 0` — **exclusive** to the corrected
   arm: a 4:2:0 stream can never bump it (`ss_x == ss_y == 0` is required),
   and no other caller sizes an intrabc rect block's chroma;
3. the 4:2:0 twin is **byte-exact** against the oracle (identity arm, run
   first and in full);
4. a **per-plane exact PREFIX** against the oracle in decode order —
   `[307200, 51360, 51360]`. This is a progress gate and the gate's own doc
   says so; asserting "wrong by exactly N" would encode r3's open defect as
   expected behaviour.

### Mutations (release profile, arithmetic only reverted; counters and gate kept)

```
MUT   the two arithmetic lines back to `(px / 2, py / 2)` / `(bw / 2, bh / 2)`
      -> RED at stream.rs:43748
         "plane Y diverges from the oracle at sample 205264 of 307200
          (x=464, y=320)"
         test result: FAILED. 0 passed; 1 failed

MUT2  hit!(IBC_RECT_CHROMA_FOOTPRINT_444_HITS) removed
      -> RED at stream.rs:43671
         "no rect intra-BC block was sized at 4:4:4 -- the corrected footprint
          never ran, so this gate is measuring the hardcoded 4:2:0 halving
          (class gate-blind-to-feature)"
         test result: FAILED. 0 passed; 1 failed

LIVE  restored tree: ok. 1 passed
```

**MUT lands on 205264 — the exact luma ceiling the av1444rect rounds 3-4
documented** ("the LUMA plane is exact to sample 205264 (it was 164416)").
Reverting two arithmetic lines in a different lane reproduces that number to
the sample, which is independent confirmation that this is the same defect
they were measuring and that nothing else had moved it.

### Regression (release, named tests; no full suite on this box)

```
444                    16 passed; 0 failed
lossless               16 passed; 0 failed
intrabc_rect            5 passed; 0 failed
a_444_sb128             2 passed; 0 failed
a_444_intrabc_rect4     1 passed; 0 failed
a_444_lossy_rect4_inter 1 passed; 0 failed
a_444_intrabc_rect_chroma_plane_block  1 passed; 0 failed   (the new gate)
```

`cargo check -p ec-av1` warning-free.

## r5 — Reusable, for whoever takes the chroma tail

* Pair decoder and oracle units by **stream bit position**
  (`debug_bitpos()` vs `aom_reader_tell()`), not by `rng` and not by a plane
  label. Both traps cost real time here and the second one is already baked
  into the repo's existing `EC_COEFF_STEP` rung.
* The oracle's last ~36 units report an `aom_reader_tell` sentinel near 2^32.
  Treat `>= 2^31` as "no position", not as a divergence.
* Our square reader logs an `all_zero` step even for a 0-bit read, so raw
  trace-line counts overstate the symbol count by ~2x. Count `post_bit != bit`.
* The `EC_IBCTX` / `EC_IBCTX2` rungs are left in the tree, env-gated, and are
  the tool for the remaining chroma tail.

---

# r2 — the chroma tail: a CORRECTION, and the attribution table

## r2.0 First: r1's "symbol-exact" claim was WRONG. It is not symbol-exact.

r1 and r3 claimed the coefficient ladder was bit-exact against the oracle
"for all 2359 oracle units that report a real bit position". **That claim is
false and must not be relied on.** The measurement that produced it was a
lockstep walk `for i in range(min(len(aom), len(our)))` comparing entry bits.
Our trace emits **3999** `all_zero` lines for the oracle's **2395** units, and
**2673 of ours consume zero bits** (the square `read_coeffs` reader logs a step
even for a 0-bit read — the trap this same report warned about in r5, which I
then failed to apply to my own walk). The walk therefore paired our *0-bit*
trace lines against the oracle's *real* units, and "2359 index positions
matched" said nothing about symbol count.

The correct measurement, counting only reads that consumed bits
(`post_bit != bit`):

```text
oracle all_zero reads              2395   (2359 with a real position)
ours  all_zero reads that consumed 1326   bits
```

**We perform 1326 real `all_zero` symbol reads where the oracle performs
2395 — roughly 1069 reads short.** The entropy ladder is *not* exact. The
footprint fix removed one real fork (and made luma exact); a second one
remains. Everything in r1/r3 that says "the remaining divergence is
reconstruction, not bitstream" is retracted.

Two lessons, both now in the managed skill: apply the 0-bit filter to the
*pairing* and not only to the census, and never report a lockstep index match
as a symbol-count match.

## r2.1 Why the bit-position key cannot finish the job on its own

The oracle's `EC_ECDUMP_IN` prints only the **entry** position
(`8 * (bptr - buf) - (cnt + 15)` at unit entry). There is no post position, and
`rng` is the coder range, not a position. So the only oracle-side key is the
entry bit, and entry bits alone cannot be lockstepped here: our per-unit bit
consumption differs from the oracle's, so the offset drifts immediately.
Measured: filtering both sides to real reads gives a first-unit offset of
**+32** (not the +15 the mixed walk reported) and the offset is already wrong
by index 1 (oracle 45 vs ours 49).

**The unblock is one line of oracle instrumentation, outside the repo:**
add the post-read position to the `EC_ECDUMP_IN` rung in
`~/.cache/aom-oracle/src/av1/decoder/decodetxb.c` (print
`aom_reader_tell(r)` after the `aom_read_symbol` at `decodetxb.c:158`). Then
pair on the **(entry, post) interval**, which is exact and collision-free, and
the next real fork is one script away. I did not make that change: it is the
shared oracle build, and rebuilding it mid-wave would move every other lane's
binary.

## r2.2 Per-sample attribution (the deliverable)

`444_intrabc_rect4_witness.obu`, frame 0, against the oracle's
`EC_AV1_FINAL_DUMP`. 8x8-cell granularity, 4-neighbour connected components
over the union of the U and V difference sets:

| | differing | first differing sample | ours | oracle |
| --- | --- | --- | --- | --- |
| Y | **0** of 307200 | — | — | — |
| U | 72945 of 307200 | (x=160, y=80) | 55 | 54 |
| V | 64865 of 307200 | (x=160, y=80) | 34 | 32 |

Components (U ∪ V, 8x8 cells):

| cells | x range | y range | shape |
| --- | --- | --- | --- |
| 8 | [160, 192) | [80, 96) | 4x2 cells = 32x16 px |
| 1182 | [320, 640) | [160, 480) | 40x40 cells = 320x320 px |

The dominant component is the whole bottom-right quadrant from (320, 160) —
the shape of a desync that starts in the small component at (160, 80) and
then runs away, not the shape of a per-block prediction error. The error
magnitudes at the first divergence are small (1 and 2 of 255) only because
the run-away has not yet accumulated there.

**So the next fork is at or just before U(160, 80) = mi (20, 40)**, and given
r2.0 it is an entropy fork, not a reconstruction one. The r3 candidate list in
this report is therefore **withdrawn**: the `override-slot-on-one-arm` replay
theory predicted a reconstruction-only divergence and is ruled out by the
symbol count.

## r2.3 What is eliminated, and what is not

Ruled out by measurement:

- **A second halved chroma footprint on this witness.** The `EC_IBCBLOCK` rung
  added in this round prints every `decode_intrabc_rect` block's
  `bw/bh/cw/ch/cside`; after the fix every 4:4:4 block's `cw, ch` equals its
  `bw, bh` (`32x8 -> 16x4`, `16x32 -> 8x16`, `8x16 -> 4x8`, all with
  `ss_x = ss_y = 0`). No halved footprint remains on this stream.
- **A 4:2:0 regression.** The pinned twin `420_intrabc_rect4_witness.obu` is
  byte-exact against the oracle in every gate run; `>> ss_x` is the same
  arithmetic as `/2` there.
- **Token-order blowup.** Confirmed not a blowup — the raw 3999-vs-2395 line
  count is 2673 zero-bit artifacts. The real gap is 1326-vs-2395, which is
  *reads missing*, not reads duplicated.

Still open, in the order I would test them next:

1. **A chroma unit read that this decoder never reaches at all.** 1069 missing
   `all_zero` reads on a stream whose luma is byte-exact is a *unit-count*
   problem, not a context problem: some chroma units libaom codes are not being
   read. The natural suspect is `decode_rect4_16_strip` and
   `read_chroma_coeffs_rect` (the two other sites still emitting 4x8/8x4 chroma
   units in the post-fix trace), and the 1:4 chroma pair geometry at 4:4:4.
2. `decode_intrabc_owned_rect` (`decode.rs:16221`) and `decode.rs:19294` still
   carry `let (cw, ch) = (bw / 2, bh / 2);` — unreachable on this witness, but
   the same latent defect and worth a sweep by whoever owns them.

## r2.4 Invariant batch (release, run in one batch after r2's probes)

See the commit message for the tails. Summary: `444` 16 passed, `lossless` 16
passed, `intrabc_rect` 5 passed, the 4:2:0 identity arm byte-exact, the new
gate green. Nothing green moved.

---

# r3 — REWORK status: items 1 and 2 done, item 3 done by construction, item 4 NOT VERIFIED

State honestly: **this branch is not ready to land and nothing is committed
for r3.** Details, because the difference matters.

## Done

**(1) The gate doc no longer asserts the retracted claim.** `stream.rs`'s
`a_444_intrabc_rect_chroma_plane_block_is_the_block_footprint` doc now states
the corrected finding: the ladder is NOT exact; the entry-position pairing was
invalid because 2673 of our trace lines consume zero bits and the two lists
differ in length; the counts, MEASURED on this branch with the 0-bit filter on
BOTH sides:

```text
oracle all_zero reads               2395   (2359 with a real position)
our   all_zero lines               3999
our   lines that consumed bits      1326
1:1 entry-bit matches, monotone merge  71
```

and it states the requirement explicitly — **pair by BIT INTERVAL, not by
index** — with the one-line oracle instrumentation that unblocks it
(`aom_reader_tell(r)` after the `aom_read_symbol` at
`~/.cache/aom-oracle/src/av1/decoder/decodetxb.c:158`).

**NOTE, and this is a correction to a figure relayed to this lane:** the
"1910 units matching 1:1 with 486 vs 486 non-zero-bit reads" number does NOT
reproduce here. This branch measures 1326 real reads and 71 interval matches.
I have written the branch's own measurement into the doc rather than the
relayed one, and flagged the discrepancy, so the next reader re-measures
instead of inheriting either figure.

**(2) The silent skip is routed.** The pixel arm's
`if !aomdec_path().is_file() { eprintln!(...); return; }` is now preceded by
`have_aomenc()`'s exact shape, so with `EC_AV1_REQUIRE_AOMENC=1` a missing
aomdec FAILS instead of reporting GREEN with the compare skipped:

```rust
let present = aomdec_path().is_file();
assert!(
    present || std::env::var_os("EC_AV1_REQUIRE_AOMENC").is_none(),
    "EC_AV1_REQUIRE_AOMENC is set but no aomdec at {} -- run scripts/build-aom-oracle.sh",
    aomdec_path().display()
);
```

It is written to the same shape main's `have_aomenc()` uses, so it merges
trivially when `lane-av1oracleskip`'s shared `aomdec_available(name)` lands.

**(3) The rustfmt churn is gone, BY CONSTRUCTION rather than by split.** The
earlier fix commit reflowed `mu_chunk_order`'s signature and a
`decode_inter_block` comparison. Rather than splitting the hunk out, r3 was
rebuilt **from main's copy of the files**: `git checkout main --
crates/ec-av1/src/{decode,stream,transform}.rs` and then re-applying the
change set by exact anchor. `git diff main -- crates/ec-av1/src/decode.rs` now
touches zero `mu_chunk_order` / `read_intra_chroma_lossless` lines. The only
removed lines in the whole decode.rs diff are the `lane-whtshape` `if lossless`
conditional that this lane unscopes, plus the two trace-format strings this
lane extends.

## NOT DONE — and why

**(4) The post-rebase gate run is not verified.** `git rebase main` CONFLICTED
on both `decode.rs` and `stream.rs`, so I aborted it and rebuilt from main's
copies of the three files. The result is a tree that is **old base
(`a21f3680`) + three files from `main` + this lane's edits** — `git diff main
--stat` shows 73 files changed and 9897 deletions, i.e. this worktree is
missing the ~70 files the other wave-3 merges added, including gates and
fixtures.

An A/B on that tree is meaningless, and I am not going to report it as one:

```text
MAIN ONLY (stashed)        ok. 25 passed; 0 failed  (713 tests)
MAIN + MY CHANGE          FAILED. 36 passed; 12 failed  (744 tests)
```

The 12 failures are all gates whose pins or scripts live in files this
half-rebased tree does not have (`a_444_lossy_superres_*`,
`a_444_lossy_rect4_strip_*`, `a_pinned_444_rect_inter_*`,
`a_real_aomenc_lossless_444_*`, `a_444_lossless_sb64_*`), and the test count
moving 713 -> 744 from a change that adds one test confirms the tree is not
what I think it is. **I have not established that my change is regression-free
on main**, and I will not claim it.

The single gate this lane owns, on the same tree, IS green:

```text
a_444_intrabc_rect_chroma_plane_block  ok. 1 passed; 0 failed
    14 rect intra-BC block(s) sized at 4:4:4; 4:2:0 twin byte-exact;
    4:4:4 luma byte-exact (307200/307200), U and V byte-exact to sample 51360
a_444_intrabc_rect4                    ok. 1 passed; 0 failed
lossless_tx_tests                      ok. 5 passed; 0 failed
cargo check -p ec-av1                 0 errors, 0 warnings
```

## The exact next step for whoever finishes this

A real `git rebase main` resolving both conflicts **by hand, hunk by hunk**,
then the invariant batch. The conflict surface is known and small on my side:
`decode_intrabc_rect`'s `cpx/cpy` + `cw/ch` block (main carries
`lane-whtshape`'s `if lossless` conditional; this lane unscopes it), the
`INTRABC_RECT_SKIPPED_HITS` thread_local block (main carries
`lane-whtshape`'s counter next to it), and the tail of `mod tests` in
`stream.rs` (main has ~30 gates this branch's base never saw). The `fixtures/`
directory is gitignored and points at a durable store, so re-checking it out
from main is what brings the other lanes' pins back.

## Provenance note

`lane-av1oraclepost` independently localised the same fork (the U `txb_skip`
at mi (92, 104), from the same hardcoded halving) and has agreed to drop its
duplicate hunk in favour of this one. **This branch is the canonical fix for
that route**; the other branch keeps its oracle instrumentation (the
post-position rung, which is also what unblocks r2's entropy fork) and its
class sweep.

---

# r3 — REWORK COMPLETE. Rebuilt on main's full tree, A/B is real, branch is green.

## The tree problem, and the fix

`git rebase main` conflicted on both files. The first attempt at the rework
rebuilt from main's THREE files, which left the worktree as *old base
(a21f3680) + 3 files from main + my edits* — `git diff main --stat` was 73 files
/ 9897 deletions, and the A/B on it was meaningless (main-only 25/0 vs
main+change 36/**12 failed**). I refused to report that as an A/B, and that
refusal was the right call: the test count moved 713 -> 744 from a change that
adds ONE test, which is only possible if the tree is a mixture.

The recipe that worked — **rebuild from the current tree, never replay
commit-by-commit**:

1. patch each of my files against main (`git diff main -- <file>`), saved to
   `/tmp/{d,s,t}.patch` — `t.patch` came out EMPTY, confirming I make no change
   to `transform.rs`;
2. `git checkout -B lane-ibc444c-r3 main`, so the WHOLE tree is main's
   (fixtures included: 48 -> 70 committed pins came back);
3. `git apply --3way` the two patches;
4. **and then reject the result of step 3 for `stream.rs`.** The 3-way apply
   put back the OLD base's text wherever the patch's context overlapped, which
   silently restored two things other lanes had removed: 167 lines of
   `gate_coverage.rs` (the `no_tool_presence_check_outside_its_probe` guard)
   and 83 lines of `lanes/av1oracleSkip.report.md`. I caught it because
   `stream.rs` showed **489 deletions** for a change that adds one test, then
   rebuilt `stream.rs` from main's copy and appended only my gate.
4. `git checkout main --` the two files step 3 had reverted.

Final state, verified before anything was run:

```text
$ git diff main --stat
 crates/ec-av1/src/decode.rs | 102 +++++++++++++++++++-----
 crates/ec-av1/src/stream.rs | 185 ++++++++++++++++++++++++++++++++++++++++++++
 2 files changed, 267 insertions(+), 20 deletions(-)

$ git diff main -- crates/ec-av1/src/decode.rs | grep -cE 'mu_chunk_order|read_intra_chroma_lossless'
0
```

Only my two files, +267/-20, and **zero** rustfmt-churn lines — item 3
verified on a real tree rather than asserted. The only removed lines are the
`lane-whtshape` `if lossless` conditional this lane unscopes (its comment block
plus the two branches) and the two trace-format strings this lane extends.

Main had also moved to `235416a6` ("Merge lane-av1oracleskip @ 7bf95961 — one
aomdec_available() for 18 guard sites, 7 redundant else skip-arms deleted"),
so the branch now rebases onto the tree that HAS `aomdec_available(name)`, and
the gate's guard calls it instead of a second hand-rolled shape. The crate ends
with ONE presence-check shape.

## The A/B, on a tree whose --stat is clean

Both halves rebuilt (`touch` before each) in the same target dir:

```text
                        MAIN ONLY              MAIN + THIS LANE
444                     ok. 34 passed; 0 failed    ok. 35 passed; 0 failed
lossless                ok. 23 passed; 0 failed    ok. 23 passed; 0 failed
intrabc_rect            ok.  5 passed; 0 failed    ok.  6 passed; 0 failed
```

**The 12 failures VANISH on a clean tree. They were tree artifacts, not
regressions** — every one was a gate whose pin or script lived in a file the
half-rebased tree lacked (`a_444_lossy_superres_*`, `a_444_lossy_rect4_*`,
`a_pinned_444_rect_inter_*`, `a_real_aomenc_lossless_444_*`,
`a_444_lossless_sb64_*`). The +1 in `444` and `intrabc_rect` is my gate. No
test that was green on main is red here.

Full batch on the committed tree (release, files touched before the build):

```text
a_444_intrabc_rect_chroma_plane_block  ok. 1 passed; 0 failed
    14 rect intra-BC block(s) sized at 4:4:4; 4:2:0 twin byte-exact;
    4:4:4 luma byte-exact (307200/307200), U and V byte-exact to sample 51360
a_444_intrabc_rect4                    ok. 1 passed; 0 failed
lossless_tx_tests                      ok. 5 passed; 0 failed
gate_coverage                          ok. 13 passed; 0 failed
refusal_inventory                      ok. 16 passed; 0 failed
444                                    ok. 35 passed; 0 failed
lossless                               ok. 23 passed; 0 failed
intrabc_rect                           ok. 6 passed; 0 failed
cargo check -p ec-av1 --all-targets    0 errors, 0 warnings
```

Red-before is unchanged in substance: reverting only the two arithmetic lines
makes the gate's luma floor bite at sample **205264** — the exact ceiling
`lanes/av1444rect.report.md` rounds 3-4 documented.

## CORRECTION — an unverified figure that must not survive as quotable

A figure of **"1910 units matching 1:1, with 486 vs 486 non-zero-bit reads"**
was relayed to this lane (source: the review's brief, via Main) and **does not
reproduce here**. Measured on this branch, with the 0-bit filter applied to
BOTH sides:

```text
oracle all_zero reads                 2395   (2359 with a real position)
our   all_zero lines                 3999
our   lines that consumed bits        1326
1:1 entry-bit matches, monotone merge   71
```

**I measure 1326 real reads and 71 interval matches.** The gate doc carries
this branch's own numbers and flags the discrepancy, so the next reader
re-measures instead of inheriting either figure. Neither number should be
quoted without re-running the measurement; writing an unverified figure as fact
is the exact mistake r2 retracted.

## The one open item, unchanged

The chroma tail is a second ENTROPY fork: ~1069 real `txb_skip` reads are
missing versus the oracle, first visible in U and V at **(x=160, y=80) =
mi (20, 40)**, spreading to the whole x[320,640) y[160,480) quadrant. The gate
pins a measured per-plane exact prefix and says so. The unblock is still the
one line of oracle instrumentation outside the repo — print
`aom_reader_tell(r)` after the `aom_read_symbol` at
`~/.cache/aom-oracle/src/av1/decoder/decodetxb.c:158` — then pair on the
`(entry, post)` bit INTERVAL, which is exact and collision-free. Coordinate
the rebuild; every lane's oracle binary moves.

## Provenance

`lane-av1oraclepost` independently localised the same fork (the U `txb_skip` at
mi (92, 104), from the same hardcoded halving) and has agreed to drop its
duplicate hunk in favour of this one. **This branch is the canonical fix for
that route**; that branch keeps its oracle instrumentation (the post-position
rung, which is also this lane's r2 unblock) and its class sweep.

---

# r4 — moved onto `f33b9d41` (wave3c-b), A/B real and green

Main advanced again, so r3's tree went stale the same way (it was based on
`0dfdf0c8` and `git diff main --stat` showed 47 files / 6984 deletions — the
`scripts/pin-gate-audit.py`, `scripts/verify-fixture-library.sh` and ~45 other
files the waves had added). Same recipe, and the same two hazards caught:

1. patch each of my files against my own base — `t.patch` EMPTY again, so I
   make no change to `transform.rs`;
2. `git checkout -B lane-ibc444c-r4 f33b9d41` — whole tree becomes current
   main, fixtures included (70 -> **85** committed pins), `aomdec_available`
   present;
3. `git apply --3way` the **decode.rs** patch only. **The stream.rs patch was
   not applied**: last round the 3-way silently restored what other lanes had
   removed (167 lines of `gate_coverage.rs`, 83 lines of
   `lanes/av1oracleSkip.report.md`), showing up as 489 deletions for a one-test
   change. stream.rs was rebuilt from `f33b9d41`'s copy with only the gate
   appended — the reliable form, and the gate guard calls main's shared
   `aomdec_available(NAME)`, so the crate keeps ONE presence-check shape.

## Item 3 and item 4, verified before anything ran

```text
$ git diff main --stat
 crates/ec-av1/src/decode.rs | 106 ++++++++++++++++++++-----
 crates/ec-av1/src/stream.rs | 185 ++++++++++++++++++++++++++++++++++++++++++++
 2 files changed, 271 insertions(+), 20 deletions(-)

$ git diff main -- crates/ec-av1/src/decode.rs | grep -cE 'mu_chunk_order|read_intra_chroma_lossless'
0

$ git diff main --stat -- scripts/ | wc -l
0
```

Two files, no deletions outside them, zero rustfmt churn, no `scripts/` touched.

## The straggler twins — NAMED, not merged

The class sweep for a second hardcoded halving inside the `decode.rs` this
patch touches finds two survivors, and I am reporting them rather than folding
them in silently (they are not on this lane's witness, so merging them would be
an unmeasured change to another path):

| line | function | note |
| --- | --- | --- |
| `decode.rs:16758` | `decode_intrabc_owned_rect` | the SAME hardcoded `let (cw, ch) = (bw / 2, bh / 2);`. Unreachable on this lane's witness (the `EC_IBCBLOCK` rung shows it is never entered there), but the same latent defect on the same class of 4:4:4 stream. This is the one I would fix first. |
| `decode.rs:19846` | `cfl_ac_q3_at` | the same expression, in a different caller family. |

The 3-way apply reported **no** conflict in either region and
`decode_intrabc_rect` took the patch cleanly; no `Reach::of` twin of the shape
fixed elsewhere surfaced in it.

## The A/B, on the real tree

Both halves rebuilt in the lane-private target dir with `touch` before each
run (the shared one has served other worktrees' binaries repeatedly today).

```text
                                  MAIN ONLY (f33b9d41)     MAIN + THIS LANE
-- list                            753 tests                754 tests   (+1 = my gate)
444                                ok. 36 passed; 0 failed  ok. 37 passed; 0 failed
lossless                           ok. 25 passed; 0 failed  ok. 25 passed; 0 failed
intrabc_rect                       ok.  6 passed; 0 failed  ok.  7 passed; 0 failed
gate_coverage                      ok. 13 passed; 0 failed  ok. 13 passed; 0 failed
refusal_inventory                  ok. 19 passed; 0 failed  ok. 19 passed; 0 failed
```

**Zero failures on either half. No test green on main is red here.** The +1 in
`444` and `intrabc_rect` is this lane's gate.

Full batch on the committed tree, release, files touched before the build:

```text
a_444_intrabc_rect_chroma_plane_block  ok. 1 passed
    14 rect intra-BC block(s) sized at 4:4:4; 4:2:0 twin byte-exact;
    4:4:4 luma byte-exact (307200/307200), U and V byte-exact to sample 51360
a_444_intrabc_rect4                    ok. 1 passed
lossless_tx_tests                      ok. 5 passed
cargo check -p ec-av1 --all-targets    0 errors, 0 warnings
```

Red-before unchanged: reverting only the two arithmetic lines makes the gate's
luma floor bite at sample **205264** — the exact ceiling
`lanes/av1444rect.report.md` rounds 3-4 documented.

## CORRECTION (carried forward, unchanged)

The relayed **"1910 units matching 1:1 with 486 vs 486 non-zero-bit reads"**
figure (source: the review's brief, via Main) does not reproduce. Measured on
this branch with the 0-bit filter applied to BOTH sides:

```text
oracle all_zero reads                 2395   (2359 with a real position)
our   all_zero lines                 3999
our   lines that consumed bits        1326
1:1 entry-bit matches, monotone merge   71
```

**I measure 1326 real reads and 71 interval matches.** Neither figure should be
quoted without re-running the measurement; writing an unverified figure as fact
is the exact mistake r2 retracted, and this correction stays in the report so it
does not survive as a quotable number.

## Still open, unchanged

The chroma tail is a second ENTROPY fork: ~1069 real `txb_skip` reads are
missing versus the oracle, first visible in U and V at **(x=160, y=80) =
mi (20, 40)**, spreading to the whole x[320,640) y[160,480) quadrant. The gate
pins a measured per-plane exact prefix and says so. The unblock is the one line
of oracle instrumentation outside the repo — print `aom_reader_tell(r)` after
the `aom_read_symbol` at
`~/.cache/aom-oracle/src/av1/decoder/decodetxb.c:158` — then pair on the
`(entry, post)` bit INTERVAL, which is exact and collision-free. Coordinate the
rebuild; every lane's oracle binary moves. `lane-av1oraclepost` already holds
that instrumentation, which is the cheapest route to it.

## Provenance

`lane-av1oraclepost` independently localised the same fork (the U `txb_skip` at
mi (92, 104), from the same hardcoded halving) and has agreed to drop its
duplicate hunk in favour of this one. **This branch is the canonical fix for
that route**; that branch keeps its oracle instrumentation and its class sweep
— and its sweep should pick up the two straggler twins named above.
