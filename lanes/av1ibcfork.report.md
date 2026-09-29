# lane-av1ibcfork — the 4:4:4 intra-BC chroma fork at mi (20,40): NOT an entropy fork

Branch `lane-av1ibcfork`, **rebased onto `79b2a73f`** (was `edffad9e`).
`backup-av1ibcfork` holds the pre-rebase commit `e0a7b744`, which carried a fix
that main had already landed in a more general form — see §6.

**Headline: the count asymmetry in the ticket does not exist. The ladder is
bit-exact. The fork is a wrong-PLACE chroma write, and on current main it is
already fixed — this branch lands the instrument that proved it, plus a gate
that pins the second witness at full byte-exactness.**

---

## 1. The count asymmetry, re-measured (and it is not there)

Fixture `crates/ec-av1/fixtures/444_intrabc_rect4_witness.obu`, 1016 B,
sha256 `7fe888edf18caa74bdb415f0de643dc59963e87e0255d77191200b7a62eb2542`
(4:4:4, 8-bit, 640x480, frame 0). Oracle
`~/.cache/aom-oracle2/build/aomdec --codec=av1`, env `EC_ECDUMP_IN=1`.

```
$ grep -c ^EC_ECDUMP_IN  oracle.err   -> 2395
$ grep -c ^EC_ECDUMP_OUT oracle.err   -> 2395
$ grep -c ^EC_ECDUMP_END oracle.err   -> 2395
$ grep -c ^EC_UNITIV_IN   rebuilt.err -> 2395
$ grep -c ^EC_UNITIV_SKIP rebuilt.err -> 2395
$ grep -c ^EC_UNITIV_END  rebuilt.err -> 2395
```

Real reads only (`post_bit != bit` ours, `tell` changed oracle):

```
ours  all_zero reads that consumed bits   512
oracle all_zero reads that consumed bits  513
```

**512 vs 513, not 1326 vs 2395.** The `~1069 fewer` figure in
`lanes/ibc444c.report.md` r2 and in the ticket is an artifact of the
instrument: the rungs fire once per unit *including* the 1883 reads that
consume zero bits, and r2's own correction warned about exactly this while
applying the filter to one side only.

## 2. The oracle rung: ALREADY THERE, and proven present in the binary

The ticket's unblock was already landed by `lane-av1oraclepost` in the private
`aom-oracle2` clone:

- `decodetxb.c:149` `EC_ECDUMP_IN` — entry + `tell=`
- `decodetxb.c:172` `EC_ECDUMP_OUT` — `az=` + post-read `tell=`
- `decodetxb.c:459` `EC_ECDUMP_END` — end of the whole unit + `tell=`

Not assumed — a lane proved a rung can be absent from a staged run:

```
$ strings ~/.cache/aom-oracle2/build/aomdec | grep -oE "EC_ECDUMP_(IN|OUT|END)" | sort -u
EC_ECDUMP_END / EC_ECDUMP_IN / EC_ECDUMP_OUT
$ stat -c '%y %n' decodetxb.c build/aomdec
2026-09-29 04:49:25  src/av1/decoder/decodetxb.c     (source)
2026-09-29 05:09:35  build/aomdec                    (binary, LATER)
```

No rebuild needed; the shared oracle checkout was never touched.

## 3. What this branch lands (the instrument, not the fix)

`EC_UNITIV` — the field-for-field twin of the oracle's triple, three prints per
unit, both coefficient readers (`read_coeffs`, `read_coeffs_rect`), on **both**
exits (the all-zero early return and the full walk):

```
EC_UNITIV_IN   side=ours plane=.. mi=(..) w=.. h=.. ctx=.. bit=..            at <caller>
EC_UNITIV_SKIP side=ours plane=.. mi=(..) w=.. h=.. ctx=.. az=.. bit=.. post_bit=..
EC_UNITIV_END  side=ours plane=.. mi=(..) w=.. h=.. ctx=.. end_bit=..
```

`EC_MCWR` (in `reconstruct_mc_rect`) and `EC_MCPUSH` (in `push_mc_rect`, with
`#[track_caller]` on both). These two exist because the intra side had
`EC_PRED` (the prediction) and the **MC/inter write path had no rung at all** —
so a region whose prediction matched the oracle and whose committed samples did
not was unattributable. That gap is what the fix turned on; see §5.

`SymbolDecoder::symr_mi()` in `msac.rs`: a per-unit rung needs a mi and a
symbol read carries none. `set_symr_mi` only wrote it; the rungs need to read
it back, from a `&SymbolDecoder`.

## 4. Bit-interval pairing WORKS — and it is demonstrably NOT index/rng pairing

Three keys on the same 2395-unit frame, oracle vs ours:

| key | result |
|---|---|
| list index | **breaks at index 8** (a rect unit whose `bit=0` because the rung was off) |
| coder `rng` | unusable at this scale; `ibc444c` r1 measured 2519 non-equal blocks and an 80-run "match" at an unrelated mi — their figure, quoted as such, not re-run |
| **bit interval `(bit_in, post_bit, end_bit)`** | **exact on all 2365 in-stream units** |

Offset **K = +14**, measured from unit 0 and then asserted constant for every
later unit (our `bit` counts bits pulled from the window; the oracle's `tell`
counts the same plus the OBU header libaom counts before `aom_reader_init`;
`ibc444c` r1 saw +15 on a different fixture — so it is measured, never assumed):

```
entry-position mismatches:  30   (all at index >= 2365)
end-of-unit mismatches:     30   (same indices)
all_zero (az) disagreements: 0 of 2395
```

The 30 are **end-of-buffer padding**, not a fork: our `bit` clamps at 7992
while the oracle's `tell` keeps counting sentinel reads toward 2^32-5906
(`ibc444c` r5's "last ~36 units" artifact, 30 here).

Ladder excerpt around the first break:

```
2355 ours p1 32x32 mi=(112,80)  in=7980 post=7980 end=7980 az=1 | orc p1 (112,80) tx=3 in=7966 post=7966 end=7966 az=1
2359 ours p0 32x32 mi=(112,96)  in=7986 post=7986 end=7986 az=1 | orc p0 (112,96) tx=3 in=7972 post=7972 end=7972 az=1
2365 ours p0 16x16 mi=(112,112) in=7992 post=7992 end=7992 az=1 | orc p0 (112,112) tx=2 in=7980 post=7980 end=7980 az=1
```

**There is no first divergent symbol in the coefficient ladder, because there
is no divergence in it.** Every unit in the stream consumes the same number of
bits on both sides, and every `all_zero` agrees.

## 5. What the divergence actually was: a wrong-PLACE write

Measured on the pre-rebase tree (`edffad9e` + this lane's fix), against the
oracle's `EC_AV1_FINAL_DUMP`:

```
RED-BEFORE  Y 0/307200    U 72945/307200 first (160,80)    V 64865/307200 first (160,80)
```

Filters exonerated first:

```
oracle PREFILT vs oracle FINAL   Y 0  U 0  V 0        <- CDEF and LR inert
ours   PREFILT vs oracle PREFILT Y 0  U 72945 V 64865 <- present BEFORE deblock
```

Two 8x8-cell difference components: **8 cells at x[160,192) y[80,96)** (sharp)
and **1182 cells over x[320,640) y[160,480)** (run-away). Two components with
those shapes is the signature of ONE wrong-place write, not two defects. The
cell was **flat 55** where the oracle has a gradient — a `DC_PRED` chroma block
committed at the wrong coordinates.

**The prediction was not the problem.** `EC_PRED` vs the oracle's `EC_PREDOUT8`
on that cell:

```
ours   OUR_PRED x=160 y=80 plane=1 bw=32 bh=16 mode=6 ad=2 sum=26258
                                      row0=[54,53,53,52,50,48,46,45]
oracle chroma sum over the same 32x16 rect = 26258, first row 54 53 53 52 50 48 46 45
```

Identical, sample for sample. So the defect had to be a *later, different*
write — and `EC_MCPUSH` (the rung that did not exist before) named it:

```
EC_MCPUSH plane=1 x=160 y=80 stride=32 w=32 h=16 res_len=512 at decode.rs:17045
EC_MCPUSH plane=2 x=160 y=80 stride=32 w=32 h=16 res_len=512 at decode.rs:17056
EC_HALV ibc_owned mi=(40,80) bw=64 bh=32 skip=true
```

`decode_intrabc_owned_rect`, `if skip` arm, block `mi=(40,80)` 64x32 **skip**.
`px=320, py=160`; the arm committed a 32x16 window at `(px/2, py/2) = (160,80)`
and left its own 64x32 footprint at (320,160) unwritten. At 4:4:4
`av1_get_max_uv_txsize` is `ss_size_lookup[bsize]` (blockd.h) and the plane block
is the block's OWN footprint, so the origin and the extent were both wrong.

## 6. The rebase: main had already fixed it, more generally

`git rebase main` conflicted in exactly that function. Main since `edffad9e`:

- `5324a685` `lane-av1chromarect` — the ss-derived chroma extent and origins,
  **this function included, skip arm included**
- `2c0fd2b9` `lane-av1chromadc` — the skipped-block chroma override windowed
  per unit

`git show main:…/decode.rs` at `decode_intrabc_owned_rect`:

```rust
let (cw, ch) = (bw >> ss_x(fctx), bh >> ss_y(fctx));
let (cpx, cpy) = (px >> ss_x(fctx), py >> ss_y(fctx));
```

and the skip arm's `push_mc_rect(1, cpx, cpy, cside, cw, ch, …)` — which is my
fix, generalized from the skip arm to the whole function. **So main's version
subsumes mine and is the one kept**, for a substantive reason and not just to
avoid a conflict: my version deliberately left the non-skip arm halving, because
`lane-av1chromahalvings` r5 recorded that its ss extent makes the decode **refuse**
(a 4:4:4 32x64 chroma plane block needs a `TxbSet` this crate lacked). Main
solved that separately with a per-unit multi-unit walk
(`read_intrabc_rect_chroma_split`), so the general form became reachable and is
the correct one. My scoped version is now strictly worse: it would have left a
known-wrong shape in the arm that reads coefficients.

Resolution, honestly: **the fix is main's; the instrument is mine.** The branch
was rebuilt from `79b2a73f` and carries only the rungs, `symr_mi()`, the gate and
this report. A short comment at the two lines records the second-witness
measurement, because main's comment cites only `r512.obu` and this is an
independent confirmation on a different stream with a different failure shape.

The pre-rebase commit `e0a7b744` is kept on `backup-av1ibcfork` for the record.

## 7. State on current main: fully byte-exact

With `chromarect` + `chromadc` in place, the witness's **residual tail is gone
too** — the "(416,192) tail" this lane called a separate open defect was a
consequence of the same wrong-place write, not an independent one:

```
$ decode_probe 444_intrabc_rect4_witness.obu  vs  oracle EC_AV1_FINAL_DUMP
MAIN+mine  Y diff 0 of 307200  first (x,y) —
MAIN+mine  U diff 0 of 307200  first (x,y) —
MAIN+mine  V diff 0 of 307200  first (x,y) —
```

That correction matters: the pre-rebase branch reported the tail as open, and it
was not.

## 8. The gate

`a_444_intrabc_rect4_witness_is_byte_exact_after_the_skip_arm_footprint`
(`stream.rs`). A **second** witness, not a repeat of
`a_444_intrabc_owned_rect_strip_sizes_its_chroma_plane_block_and_decodes`
(which pins `r512.obu`) — the two shapes fail differently: `r512.obu`'s 128-root
strips fail as a sizing error *inside the coefficient walk*, while this
witness's 64x32 skip block read **no coefficient at all**, so its only symptom
was pixels.

Four arms: 4:4:4 header asserted; the 4:2:0 twin byte-exact against the oracle
(where `>> ss_x` IS `/ 2`, so it is the measurement and not a claim);
`IBC_OWNED_RECT_CHROMA_FOOTPRINT_444_HITS >= 1` (gated on `ss_x == ss_y == 0`,
so 4:2:0 can never bump it); **full byte-exactness on all three planes** — no
prefix floor, which is only possible because both landed.

```
a_444_intrabc_rect4_witness_is_byte_exact_after_the_skip_arm_footprint:
  1 rect intra-BC block(s) committed a 4:4:4 chroma plane block;
  4:2:0 twin byte-exact; 4:4:4 byte-exact on all three planes (307200 samples each)
test result: ok. 1 passed; 0 failed
```

### Mutation proof (release, the whole function back to the halving)

```
MUT  (bw >> ss_x, bh >> ss_y) / (px >> ss_x, py >> ss_y) -> (bw / 2, bh / 2) / (px / 2, py / 2)
     -> RED at stream.rs:49723
        "plane U diverges from the oracle at sample 51360 of 307200 (x=160, y=80)
         -- this gate pins FULL byte-exactness on all three planes"
     test result: FAILED. 0 passed; 1 failed

LIVE  restored: ok
```

The red lands on the **same (160,80)** this lane measured by hand before the
gate existed — the gate and the investigation agree to the sample.

## 9. Gates re-run (release, files touched before the build)

```
a_444_intrabc_rect4_witness_is_byte_exact_after_the_skip_arm_footprint  ok
a_444_intrabc_owned_rect_strip_sizes_its_chroma_plane_block_and_decodes  ok   (chromarect)
a_444_skipped_64x64_square_block_windows_its_chroma_override_per_unit   ok   (chromadc)
  -> test result: ok. 3 passed; 0 failed; 0 ignored; 769 filtered out

444 lossless intrabc_rect gate_coverage refusal_inventory census
  -> test result: ok. 109 passed; 0 failed; 3 ignored; 0 measured; 660 filtered out

mutation (halving restored, counter and gate kept)
  -> FAILED. 0 passed; 1 failed     then restored -> ok
cargo check -p ec-av1 --all-targets -> 0 errors, 0 warnings
```

Main's checkout stayed clean throughout (`git status --porcelain` in
`/home/tahinli/Documents/Code/Rust/edith_codecs` printed nothing after every
edit batch).

## 10. Files (this branch vs `79b2a73f`)

| file | what |
|---|---|
| `crates/ec-av1/src/msac.rs` | `SymbolDecoder::symr_mi()` |
| `crates/ec-av1/src/decode.rs` | `EC_UNITIV` rungs + 4 helpers; `EC_MCWR`; `EC_MCPUSH` + `#[track_caller]` on `reconstruct_mc_rect` and `push_mc_rect`; the second-witness comment at the two ss lines |
| `crates/ec-av1/src/stream.rs` | `a_444_intrabc_rect4_witness_is_byte_exact_after_the_skip_arm_footprint` |
| `lanes/av1ibcfork.report.md` | this file |

## 11. Not done, named

1. **The `~1069 fewer reads` figure is now known to be an instrument artifact**
   (512 vs 513 measured, both sides filtered). It is still stated as fact in
   `lanes/ibc444c.report.md` r2 and in the gate doc at `stream.rs:48319`
   ("the chroma tail is a second entropy fork"). Correcting another lane's
   report text is a separate edit and was not done here; this report carries the
   real number and a future reader is told to re-measure.
2. **No full local suite** — project rule: full suites run on the VPS fleet
   only. Everything quoted is a scoped, named run.
3. **The oracle's `EC_ECDUMP_END` tail sentinel** (30 units, `tell >= 2^31`) is
   still an oracle-side artifact; our side clamps, so the last 30 ladder entries
   are not comparable. Treated as "no position", per `ibc444c` r5.
4. **The two rungs are inert unless armed.** `EC_UNITIV` / `EC_MCWR` /
   `EC_MCPUSH` each hold one `LazyLock` atomic load per call site; nothing runs
   per sample on the hot path that was not already there.
