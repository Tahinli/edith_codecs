# lane-av1leftref — the left chroma reference's `+ss_y` row term

Carried open from `lanes/av1422warp.report.md` ("Known unresolved", last
bullet). One commit, no push. Worktree `~/.cache/wt/av1leftref`, branch
`lane-av1leftref`, base `4155c7c7`.

## 1. What changed

`Neighbours::smooth_uv_neighbour_unsnapped` (`decode.rs`) read the LEFT chroma
neighbour on its own mi row at every subsampling. libaom reads it one mi row
**down** wherever `ss_y == 1`:

```c
/* av1/common/av1_common_int.h:1395-1401, set_mi_row_col */
MB_MODE_INFO **base_mi = &xd->mi[-(mi_row & ss_y) * xd->mi_stride - (mi_col & ss_x)];
MB_MODE_INFO *chroma_above_mi = xd->chroma_up_available ? base_mi[-xd->mi_stride + ss_x] : NULL;
MB_MODE_INFO *chroma_left_mi  = xd->chroma_left_available ? base_mi[ss_y * xd->mi_stride - 1] : NULL;
```

`base_mi` is the block's own mi moved by `-(mi_row & ss_y)` rows and
`-(mi_col & ss_x)` columns, so the two reads are

| read | offset from `base_mi` | cell, with the caller's `(mi_row & ss_y)` snap already applied |
|---|---|---|
| above | `[-stride + ss_x]` | row `-1`, col `+ss_x` |
| left | `[ss_y*stride - 1]` | row `+ss_y`, col `-1` |

The above COLUMN (`+ss_x`) was already shipped by `a9611ea2`/`4b9c4cd8`. The
left ROW (`+ss_y`) was not — that is this lane. `left_mi` is now
`(mi_r + ss_y, mi_c - 1)`.

Consequences by format:

* **4:2:0** (`ss_y = 1`) and **4:4:0** (`ss_y = 1`): the read moves one mi row
  down. This is the only behaviour change in the lane.
* **4:2:2** (`ss_x = 1, ss_y = 0`) and **4:4:4** (`ss_x = ss_y = 0`): `ss_y == 0`,
  so `left_mi.0 == mi_r` and the read is literally the same cell as before. The
  change is a no-op there *by construction*, not by measurement.

An earlier attempt applied the `ss_y` term to the WRONG read (the above) and
regressed the byte-exact 4:2:0 control — that is what exposed the crossing.
Applying it to the left read, as libaom does, regresses nothing.

`smooth_uv_neighbour_unsnapped` gained an `ss_y` parameter and a
`decode_path: bool`. The latter matters: the 16x4/4x16 pair witness
(`decode.rs:17435`) calls the unsnapped form as a **counter-only mirror** with
the pair's raw `lmi`, which is an ODD mi row. Counting that mirror's flips
makes the lane look reachable when no committed fixture reaches it on a decode
path (see §3).

## 2. Gate

`stream.rs::a_4to20_key_frame_takes_the_left_chroma_reference_read_its_libaom_row`

```
a_4to20_key_frame_takes_the_left_chroma_reference_read_its_libaom_row:
22292 left-reference reads, +ss_y changed the mode 0 and the smooth boolean 0
times, all three planes pixel-exact
test result: ok. 1 passed; 0 failed
```

Fixture `crates/ec-av1/fixtures/hg_kf900.obu` (484640 bytes, sha256
`a273ae99328388be14a1ff8fa1c5a6d391a9948159eaef120e1b50e02015eb83`), the
densest 4:2:0 intra leaf mix in the set. It asserts the arm ran
(`uv_left_ss_y_reads() > 1000`) and that all THREE planes — not luma alone, as
the kf900 gate does — match ffmpeg. `uv_left_ss_y_mode_diff_hits` /
`uv_left_ss_y_smooth_diff_hits` are printed, not asserted: they read 0 (§3), so
asserting `== 0` would pin an accident, and a non-zero value is the first
witness rather than a failure.

## 3. Witness: unreachable, with the counter evidence

**No stream was found where the term changes the decode path's answer.** Here is
what was measured, and why.

### 3.1 The counter is correct and the arm is live

`uv_left_ss_y_reads` counts decode-path chroma edge-filter-type reads with a
left neighbour and `ss_y > 0`:

| fixture | format | decode-path reads | mode diff | smooth diff |
|---|---|---|---|---|
| `hg_kf900.obu` | yuv420p10le | 22292 | 0 | 0 |
| `gm_small_side_witness.obu` | yuv420p10le | 13311 | 0 | 0 |
| `troy_sb128_inter_witness.obu` | yuv420p10le | — | 0 | 0 |
| `hg_ss300_key_frame.obu` | yuv420p10le | — | 0 | 0 |
| `hg_ss600_key_frame.obu` | yuv420p10le | — | 0 | 0 |
| `troy_kf2700.obu` | yuv420p10le | — | 0 | 0 |
| `av112bit-{inter,compound,compound-masked}.obu` | yuv420p12le | — | 0 | 0 |

The arm runs tens of thousands of times per fixture. It simply never lands on
two different blocks.

### 3.2 The false positive that made it look reachable

Before the `decode_path` split, the same counter read **50** on `hg_kf900`,
14 on `gm_small_side_witness`, 8 on `troy_sb128_inter_witness`, 2 on
`hg_ss300`, 2 on `troy_kf2700`, 1 on `hg_ss600`, 1 each on three `av112bit`
streams. Every one of them has an **odd `mi_r`**. The decode path always snaps
to an even base row (`mi_r & !((1 << ss_y) - 1)`), so an odd row can only come
from the 16x4/4x16 pair witness's counter-only mirror at `decode.rs:17435`,
whose result feeds `RECT4_16_UV_PAIR_FILT_HITS` and nothing else. Those 50 are
not chroma edge filters.

This is the concrete reason the earlier lanes' premise ("no committed fixture
presents a 1-mi-tall left neighbour") looked true while the shape was in fact
being read constantly: it was being read at a call site that discards it.

### 3.3 Why the shape is structurally absent, not merely rare

The two cells the read compares are **vertically adjacent in one left mi
column**, so they are two different blocks only where a left neighbour is at
most 1 mi tall. Dumping the `uv_mode_grid` column at every decode-path read
(`EC_LEFTSSY` trace, since removed) shows the runs are **always even length** —
`[Some(0), Some(0), Some(0), Some(0)]`, `[Some(0), Some(0), Some(12),
Some(12), Some(12), Some(12)]`, never a run of 1. So a read at an even base row
`(2k, 2k+1)` never straddles a block boundary.

libaom only starts a block on an odd mi row with an 8-px-tall strip, and aomenc
never picks one in any recipe swept. The 4-px `HORZ_4` strips aomenc *does* pick
(a 16x16 split into four 16x4) all land **inside one mi row**, so four distinct
blocks write the same cell and the last one wins — the column still reads as one
run.

### 3.4 Sweep

Five recipe families, ~1000 successful encodes, all 4:2:0, all measured with
`uv_left_ss_y_mode_diff_hits` / `..._smooth_diff_hits`:

| sweep | sources | partition flags | cq | cpu-used | encodes | decode-path flips |
|---|---|---|---|---|---|---|
| 1 | testsrc2/testsrc/mandelbrot/gradients/cellauto 320x240 8-bit | default, `--lossless=1`, `rect+1to4+min4` | 20-60 | 0-4 | 240 | 0 |
| 2 | same, 640x480 8-bit | `sb 32/64/128`, `max-part 8/16`, `min 4` | 18-50 | 0-4 | 360 | 0 |
| 3 | same, 640x480 **10-bit** | same | 18-50 | 0-4 | 360 | 0 |
| 4 | mixed flat/textured 320x480 10-bit, **vertical** edges | same | 20-44 | 0-3 | 240 | 0 |
| 5 | mixed gradient/textured 320x240 10-bit, **horizontal** edges | same | 16-46 | 0-3 | 288 | 0 |

Sweep 4 put the texture change in the wrong axis; sweep 5 corrected it (the
comparison is a vertical pair, so it needs a *horizontal* content edge) and
used gradients rather than constant colour, because on a constant region RDO
picks `DC_PRED` (fewer bits) and `UV_SMOOTH_*` never wins — a stream can have
smooth UV modes and still never put a smooth block against a non-smooth one in
the same left column. Even at `--max-partition-size=8`, aomenc produced no
odd-length runs (§3.3).

## 4. Red / green

There is no red→green at the pixel level, because the term is unreachable; the
honest red/green is the corpus measurement and the counter.

**Mutation (the read reverted to the pre-lane own-row cell):** output is
byte-identical on all 35 committed fixtures that decode — measured ours-vs-ours
through `dump_yuv` (u16 LE on both sides, so valid at 8, 10 and 12 bit; a
dump-vs-`aomdec` cmp at 8 bit is not, and an early version of this lane's
harness was wrong for exactly that reason). 35 identical / 0 changed. The six
4:2:2 fixtures decode zero frames in both builds (the 4:2:2 refusal is
unconditional in this tree; the `EC_AV1_ALLOW_422_PROBE` bypass is not
committed), and they are a no-op by construction anyway (§1).

**Oracle exactness (10-bit, `aomdec --rawvideo`, whole stream):** byte-exact
before and after the change —

| fixture | frames | pre-lane vs aomdec | with the `+ss_y` read |
|---|---|---|---|
| `gm_small_side_witness.obu` | 33 | exact | exact |
| `hg_kf900.obu` | 1 | exact | exact |
| `hg_ss300_key_frame.obu` | 1 | exact | exact |
| `hg_ss600_key_frame.obu` | 1 | exact | exact |
| `troy_kf2700.obu` | 1 | exact | exact |
| `troy_sb128_inter_witness.obu` | 15 | exact | exact |
| `hg_intra14_witness.obu` | 1 | exact | exact |

**Non-discrimination, stated plainly:** because both reads agree on every
committed fixture, the gate in §2 would still pass with the `+ss_y` term
removed. It is a reach-and-exactness pin, not a mutation-proof pin. A gate that
discriminates needs a stream that reaches the shape, and §3 is the search for
it. `Selin-3` independently re-ran their chroma-format battery on clean
`4155c7c7` and on this build and saw no verdict move
(`lanes/av1formatsweep.report.md`).

## 5. Pre-existing defect found by the sweep, not fixed here

`gradients=s=640x480` 10-bit, `--sb-size=64 --max-partition-size=16
--min-partition-size=4 --enable-1to4-partitions=1 --enable-rect-partitions=1
--lossless=1 --cq-level=34 --cpu-used=0` decodes into
`decode.rs:2787: assertion left == right failed, left: (4, 8), right: (4, 4)`.
It panics identically on the pre-lane and the shipped build, so it is not this
lane's. Left for its owner.

## 6. Unblock for a future round

The shape needs a left neighbour that is 1 mi tall **and** whose vertical
neighbour in the same mi column codes a different `uv_mode`. libaom only starts
a block on an odd mi row via an 8-px strip, so the search should stop sweeping
partition flags and start at the encoder's 1:4 path: a 32x32 `HORZ_4` into four
32x8 strips puts strip origins on mi rows 0/1/2/3, which is the only geometry
that produces a run of length 1. aomenc selected none of those in ~1000 encodes
at `cpu-used=0` with `--enable-1to4-partitions=1`; the next thing to try is
hand-constructing the partition tree (or an aomenc patch to force
`PARTITION_HORZ_4` on 32x32), since the RDO simply never picks it.

## State

One commit on `lane-av1leftref`, nothing pushed. The `EC_LEFTSSY` diagnostic
trace and the slice dumps are removed; what remains is the read, the
`ss_y`/`decode_path` parameters, the three counters and the gate.
