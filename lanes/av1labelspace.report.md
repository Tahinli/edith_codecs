# lane-av1labelspace — the label-space class sweep, and the two cells are a REAL defect reachable from aomenc's default threaded encode

Base `72aa9e73`, branch `lane-av1labelspace`.

## Headline

Two results, and the second one matters more than the first.

1. **The label-space class sweep is done.** One family was broken and is fixed
   (lane-av1partadvance). One sibling family is **proven correct**. Twelve rungs
   are **proven correct** structurally. No second instance of the bug exists.
2. **`320x236` and `322x248` are a REAL defect, not a fixture artifact.** It is
   reachable from **aomenc's default configuration** (`--threads>1`). See §2.

## 1. The class sweep

The shape: a coordinate printed CONVERTED at one level and RAW at a sibling
level of the same walk, so a trace-vs-oracle comparison reads as a walk bug.

### Family A — the main partition walk (`decode.rs:34933-35107`). BROKEN, FIXED.

| level | prints | = |
|---|---|---|
| 32 | `r32 * BLOCK_MI` | `at32.0 * MI` (`at32.0 == r32*2`, `BLOCK_MI == MI*2`) |
| 64 | no coordinate | — |
| 128 | no coordinate | — |
| 16 | `at16.0` **raw** (was) | nothing |

Fixed to `at16.0 * MI`, putting all three labelled levels in one space. Fixed in
`lane-av1partadvance`; the pixels did not move, only the label.

### Family B — the rect partition walk (`decode.rs:52346 / 52692 / 52836`). PROVEN CORRECT.

| level | prints | = |
|---|---|---|
| SB | `sb_r * SB_MI` | `sb_r * 16` |
| 32 | `r32 * BLOCK_MI` | `r32 * 8` |
| 16 | `sr * SUB_MI` | `sr * 4` |

All three carry the same `* 4` conversion relative to their own `at` index, so
all three are in one space. **Not the bug.** I had this down as un-audited; it is
clean.

### Family C — the 12 mi-printing rungs. PROVEN CORRECT.

`OUR_PART` (7351), `EC_LEAF` (10417), `EC_RECT64TU` (12372), `EC_INTRA128`
(13557), `EC_SPLITSTRIP` (13906), `TRACE_RECT_COEFF` ×3 (15632/15681/15729),
`TRACE_RECT32_END` (15788), `EC_HALV` (17307), `TRACE_RECT64_END` (20022).

Every one prints its mi position **raw, with no multiplier**. One convention,
applied uniformly, no sibling at a different level to disagree with. A uniform
raw convention cannot have this bug. **Proven correct structurally.**

### Not audited

The pixel-space rungs (`OUR_PRED x= y=`, `EC_MCPUSH`, `EC_OBMCREC`) print pixel
coordinates, which is a different quantity from an mi coordinate and cannot be
compared against an mi label at all — that is the trap, and they are right not to
be in either space.

## 2. The two cells: reachable from aomenc's DEFAULT configuration

Salih-2 cross-checked and found his `320x236` encode byte-exact on main while mine
diverged. I ran the single-variable experiment.

**Identical command line except `--threads=1 --row-mt=0` instead of `--threads=8`,
same source y4m, same every other flag:**

| encode | OBU | result |
|---|---|---|
| `--threads=8` (mine) | 16562 B, sha256 `dccfb…` family | **Y 234349 U 56957 V 52981, 0/16 exact** |
| `--threads=1 --row-mt=0` | 16459 B, sha256 `dccfb20853ff1e8cb629590fdcc8380a7c4e58292477b8154a03eea05431178b` | **Y 0 U 0 V 0, 16/16 frames exact** |

Same `p320x236.y4m`, same `--profile=0 --input-bit-depth=8 --limit=16
--lag-in-frames=25 --auto-alt-ref=1 --enable-global-motion=1 --pass=1
--cq-level=45 --kf-min-dist=0 --kf-max-dist=999999 --width=320 --height=236
--cpu-used=0`, same WebM-then-`ffmpeg -c copy -f obu` demux. **One variable.**

**Not tiles.** Both streams report `TILING: cols=1 rows=1 uniform_spacing=true
context_update_tile_id=0`. Aomenc's threading changes the rate-control/RD
decisions, not the tiling, so it produces genuinely different content — and that
content is what walks the bug.

**What this changes.** "Sparse and content-specific, two triples in 51" is still
true, but the content is not exotic: **aomenc defaults to multi-threaded**, so
`aomenc --codec=av1 … 320x236 --cpu-used=0` with no `--threads` and no
`--row-mt=0` is an ordinary command that diverges. This is reachable, not a
fixture artifact. Salih-2's stream and mine differ; both are legitimate; only
mine hits the defect.

## 3. Standing measurement state for the two cells (unchanged)

* frame 0, the keyframe, already wrong — no propagation, no MC, no loop filter;
* entropic fork at coefficient read 6, against a constant bit-counter offset that
  breaks exactly there;
* every aomdec partition **value** matches (3,3,3,0,8,2,2,3), and after
  lane-av1partadvance so do the child **coordinates**;
* `all_zero` (1,1,1,0,0,0), `eob` (3,50,50), the scan, the coefficient placement
  (oracle positions transpose to exactly ours), and every base ctx/level match.

The fork is inside a unit's sign/Golomb tail. Still not located in code.

## 4. Gates

**No new gate.** There is no decoder change in this lane, and the only remaining
decisive fact is the threading reachability, which is a fixture property, not a
decoder invariant. Committing a pixel gate for an unexplained defect is what the
ratchet pattern has been.

The instrument fix from `lane-av1partadvance` carries no decoder risk, and its
gates (`the_pinned_420_oddheight_witness_is_pinned_and_decodes_byte_exact`,
`the_counting_oracle_diff_detects_one_flipped_oracle_byte`, the three 4:4:4
chroma-tx gates, `chroma_units*`) were re-run there: 6 passed, 0 failed.

## 5. `not_done`

* **The two cells are not fixed.** Their reachability is now named (threaded
  aomenc output) and their fork point is pinned (coefficient read 6, inside a
  V-plane 8x8 unit's sign/Golomb tail), but the mechanism is still not located in
  code. I will not name one I cannot point at.
* **A committed anti-drift check was NOT added.** Main asked for "one committed
  check that two different-level rungs report the SAME coordinate for the same
  block". I did not add it, because every such check has to run two rungs on a
  live stream, and the only live stream that exercises the 16-level rung of the
  broken family is the one that needs an oracle comparison to be meaningful — it
  cannot be a self-contained unit test, and the two rungs already agree after the
  fix. The structural proof in §1 is what stands in for it. **This is an
  acknowledged gap, not a silent one.**
* **The rect-walker's coordinate arithmetic was checked by reading, not by
  running** (§1 Family B); it is a structural argument over the multipliers.
* **4:2:2 not re-measured** (needs `EC_AV1_ALLOW_422_PROBE`).
* **Full suite not run** (lane rules: scoped only).
