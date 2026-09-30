# lane-av1oddgolomb — the deciding measurement: candidate 1 is REFUTED, and the real fork is the partition walk's child advance

Base `0224d10c`, branch `lane-av1oddgolomb`. **No code change.**

## Verdict

**Candidate 1 (sign / Golomb tail) is refuted, and it is refuted by the
partition walk, not by the tail.** Every partition VALUE matches the oracle
symbol-for-symbol. The fork is that after a 32x32 `PARTITION_SPLIT`, our decoder
advances its 16x16 children by **4 px** where libaom advances by **16 px**. That
is the whole of the 312 bits: our walk re-enters the interior of the block it just
decoded, where it then reads a `PARTITION_HORZ_4`, a `TRACE_RECT_PREFI` and a whole
palette block that libaom never reads at that point.

## 1. Red-before + control (unchanged from lane-av1oddluma)

| cell | Y | U | V | exact |
|---|---|---|---|---|
| `320x236` | 234349 | 56957 | 52981 | 0/16 |
| `322x248` | 36286 | 13359 | 13001 | 0/16 |

`EC_FLIP` byte 0 → `+1 Y` on each. Frame 0 (keyframe) already wrong on both.

## 2. The measurement

Oracle `EC_TRACE` (`EC_PART` / `EC_PART_VAL`) against our `EC_AV1_TRACE`
(`TRACE partition_w128 / w64 / w32 / w16`), frame 0, in order:

| # | oracle (mi in 4-px units) | value | ours | value |
|---|---|---|---|---|
| 1 | mi(0,0) bsize=15 | 3 | `partition_w128 ctx=0` | 3 |
| 2 | mi(0,0) bsize=12 | 3 | `partition_w64 ctx=0` | 3 |
| 3 | mi(0,0) bsize=9 | 3 | `partition_w32 mi=(0,0)` | 3 |
| 4 | mi(0,0) bsize=6 | 0 | `partition_w16 mi=(0,0)` | 0 |
| 5 | **mi(0,4) bsize=6** | **8** | **`partition_w16 mi=(0,1)`** | **8** |
| 6 | **mi(4,0) bsize=6** | **2** | **`partition_w16 mi=(1,0)`** | **2** |
| 7 | **mi(4,4) bsize=6** | **2** | **`partition_w16 mi=(1,1)`** | **2** |
| 8 | mi(0,8) bsize=9 | 3 | `partition_w32 mi=(0,8)` | 3 |

**Every value matches: 3, 3, 3, 0, 8, 2, 2, 3.** The entropy decisions are right
and the context is right (`tell`/`rng` agree up to the walk error).

Step 3 is `PARTITION_SPLIT` at `BLOCK_32X32`, whose four 16x16 children in libaom
sit at 16-px offsets: mi(0,0), mi(0,4), mi(4,0), mi(4,4). Ours reads them at
mi(0,0), mi(0,1), mi(1,0), mi(1,1) — **1 mi = 4 px** of stride where there must be
4 mi = 16 px.

Step 8 agrees (`mi(0,8)` both sides, the 32x32 sibling 32 px away), so the
**32x32-level advance is correct and the 16x16-level advance is not.** The bug is
in the child-advance of one partition arm, not in the whole walk.

### Why this is the 312 bits

Immediately after our V-plane unit's `TRACE eob value=50` our trace goes straight
to

```
TRACE dequant plane=2 base_q_idx=21 tx_type=DctDct side=8 levels=[0, 0, 1, 0, …]
TRACE partition_w16 mi=(0,1) ctx=0 value=8
TRACE_RECT_PREFI mode=12 uv_mode=0 … rng=57864
EC_PAL row=0 col=0 ctx=-1 n=2 rng=57864
```

— no `TRACE dc_sign` / `TRACE golomb` lines for that unit at all. Meanwhile
libaom, for the SAME `eob=50`, reads a full `tag=sign` / `tag=post_golomb` tail
(`c=3,4,5,12,47,49` at level 1, then `c=0 sign=1 dcctx=2 level=4`).

So the sign/Golomb difference I was chasing is a **consequence**: libaom's walk
leaves the mi(0,0) leaf for mi(0,4), while ours stays inside it, and the two then
read completely different symbol streams. The tail is not where the bits are lost.

## 3. The site, and what is still owed

The named site is **the child-advance after a `PARTITION_SPLIT` at
`BLOCK_32X32` in `crates/ec-av1/src/decode.rs`'s partition walk** — the code that,
on reading a 32x32 partition symbol, steps its 16x16 child cursor. The 16x16-level
advance must be the block's own 16-px mi stride (4 mi), not 1 mi.

**Deciding measurement still owed before any edit** (one `grep` + one read, which I
ran out of budget for): find the `PARTITION_SPLIT` arm that advances a 32x32
cursor, and compare its stride against the `PARTITION_HORZ`/`VERT` arms of the same
function, which step 32x32 correctly. The suspicion is one stride expression
computed from the wrong block size (the parent's 32 instead of the child's 16, or
a `BLOCK_32X32` constant where the child's `mi_size_wide` belongs). **I have not
located the line and I am not editing blind.**

Note the entropy is still exact through the wrong walk — every partition value
matched — which means the decoder does not detect the mis-step. That is why it is
silent rather than a corruption.

## 4. Sweep — no trigger rule claimed

27-cell grid: 25/27 exact; `320x236` and `322x248` fail. The extended grid
(heights 260/264/268/272, widths 318/326/330, 12 more encodes, all on disk in
`/tmp/h232fix`) was **not** swept this lane — I am not quoting numbers I did not
measure. The "dimension not a multiple of 8" hypothesis remains **FALSE**
(324 is 4 mod 8 and `324x240` is exact; 236 is 4 mod 8 and `320x236` fails).

## 5. Class sweep (which other arms carry the same derivation)

Unmeasured, and therefore not claimed: I established that the 32x32-level advance
is correct and the 16x16-level one is not **for `PARTITION_SPLIT` at
`BLOCK_32X32` on `320x236`**. I did not sweep the other partition types
(`HORZ`/`VERT`/`HORZ_A`/`HORZ_B`/`VERT_A`/`VERT_B`/`HORZ_4`/`VERT_4`) or the other
block sizes, and the correct ones among them are not identified. Whoever fixes this
should gate it with the partition census (`crate::census`) rather than assume the
sibling arms are right.

## 6. `not_done`

* **Not fixed.** No decoder change; the tree matches `0224d10c`.
* **The exact line is not located.** §3 names the arm, not the expression.
* **No gate committed.** A gate cannot be written until the fix lands, and I will
  not commit a ratchet for a mechanism I have not identified in code.
* **Extended grid not swept** (§4); fixtures are on disk.
* **Other partition arms not surveyed** (§5).
* **4:2:2 not re-measured** (needs `EC_AV1_ALLOW_422_PROBE`).
* **Full suite not run** (lane rules: scoped only).
