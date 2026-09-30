# lane-av1partadvance — it is a LABEL defect, not a stride defect; the two cells are unexplained again

Base `2d8851c0`, branch `lane-av1partadvance`.

**One change, and it is an instrument fix: the 16-level partition rung's label
was in a different label space from the 32-level rung's, 4x apart. The walk was
already correct. The pixels did not change — which is the proof.**

## 1. Which is the label? — answered first, as instructed

`crates/ec-av1/src/decode.rs`, the 32-level arm of the partition walk:

```rust
let at32 = (r32 as usize * 2, c32 as usize * 2);          // 34938
...
eprintln!("EC_PART mi_row={} mi_col={} bsize=9 ...",
          r32 * BLOCK_MI, c32 * BLOCK_MI, ...)            // 34947-34953
eprintln!("TRACE partition_w32 mi=({},{}) ...",
          r32 * BLOCK_MI, c32 * BLOCK_MI, ...)            // 34967-34970
```

The walk's own mi position is `at32 = (r32*2, c32*2)`; the printed label is
`r32 * BLOCK_MI = r32 * 8 = at32.0 * MI`. **So the convention is `at_mi * MI`.**

The 16-level arm, 130 lines down, in the SAME walk:

```rust
let at16 = (sr, sc);                                       // 35069
...
eprintln!("EC_PART mi_row={} mi_col={} bsize=6 ...", at16.0, at16.1, ...)   // 35074-35080
eprintln!("TRACE partition_w16 mi=({},{}) ...", at16.0, at16.1, ...)       // 35091-35092
```

`at16.0` — **raw, with no `* MI`.** Same walk, same `at_mi` quantity, two label
spaces, 4x apart. That is the whole defect.

Lane-av1oddgolomb compared `TRACE partition_w16 mi=(0,1)` (our space) against
aomdec's `EC_PART mi_row=0 mi_col=4` (the converted space) and read a 4x child-
advance bug. It is an artifact of comparing across label spaces — the class this
crate has been bitten by twice already this wave.

**Answer to "which is it":** a 4-px-mi-derived index, printed unconverted at the
16 level and converted at the 32 level. Not a child index, not px, not a parent
plus offset.

## 2. The fix

One line per rung, three rungs (the two `EC_TRACE` twins and the `EC_AV1_TRACE`
one), all inside `env_flag!` blocks:

```rust
let (lbl_r, lbl_c) = (at16.0 * MI, at16.1 * MI);
```

The walk — `sr, sc`, `at16`, the bounds check at 35060 that already used
`(sr as u32) * SUB_MI`, `has_half(sc as u32 * SUB_MI, …)`, and every
`decode_block` call — is **untouched**. Only what the rungs print changed.

## 3. Proof, independent of the trace

**Label moved, to the oracle's own value, rng included.**

| | before | after |
|---|---|---|
| ours, 2nd 16-level symbol | `TRACE partition_w16 mi=(0,1) ctx=0 value=8` | `EC_PART mi_row=0 mi_col=4 bsize=6 ctx=0 tell=688 rng=39176` |
| oracle | `EC_PART mi_row=0 mi_col=4 bsize=6 ctx=4 tell=674 rng=39176` | same |

The **msac `rng` matches exactly** (39176), and so do the first symbol
(`mi=(0,0) … value=0`, rng 54632) and the 32-level sibling. Only `tell` differs,
by the same constant bit-counter offset seen throughout this wave.

**Pixels did not move.** Same comparator, same oracle, before and after:

| cell | before | after |
|---|---|---|
| `320x236` | Y 234349 U 56957 V 52981 | **Y 234349 U 56957 V 52981** |
| `322x248` | Y 36286 U 13359 V 13001 | **Y 36286 U 13359 V 13001** |

Identical. That is what makes this an instrument fix and **not** a decoder fix:
a label fix must change the label and nothing else, and this one does.

## 4. The finding, stated honestly: the two cells are UNEXPLAINED again

**The mechanism I reported last lane does not exist.** `320x236` and `322x248`
diverge, are entropic, and fork somewhere I have **not** located. Everything I
measured still stands and now rules out more than it did:

* frame 0 (the keyframe) is already wrong on both — no propagation, no MC, no
  loop filter;
* the fork is at coefficient read 6 of frame 0, inside the read-5 (V-plane 8x8)
  unit's tail, against a constant bit-counter offset that breaks exactly there;
* every partition **value** matches aomdec symbol-for-symbol (3,3,3,0,8,2,2,3);
* the coefficient scan, the coefficient **placement** (the oracle's positions
  transpose by `(p%8)*8 + p/8` to exactly ours), `all_zero`, `eob`, and every
  base ctx/level match;
* **and now the partition child coordinates match too.**

So the fork is inside a unit's sign/Golomb tail, not in any table, scan or walk
step I have looked at. I am not going to name a mechanism I cannot point at.

## 5. Sweep — 51 cells, and the rate is 2

Same comparator throughout, `EC_FLIP` +1 control on every failing cell.

**Core grid re-run (27 cells), unchanged: 25/27 exact.** `320x236` and `322x248`
still fail, byte-identically. `320x232` spot-checked still 0/0/0, 16/16 frames.

**Extended grid (24 new cells), all 24 EXACT:**

* heights 260, 264, 268, 272 x widths 320, 322, 324 — 12/12 exact
* widths 318, 326, 330 x heights 232, 236, 240, 248 — 12/12 exact

**49 of 51 cells exact. The rate is 2/51.**

The geometric hypothesis is now definitively dead, and the extended grid kills it
harder than the core one could:

* **height 236 is not the trigger.** `318x236`, `324x236`, `326x236`, `330x236`
  and `322x236` are all EXACT; only `320x236` fails.
* **width 322 is not the trigger.** `322x232`, `322x236`, `322x240` are exact;
  only `322x248` fails.
* **"not a multiple of 8" is not the trigger.** 318, 322, 324, 326, 330 and 236
  are all non-multiples of 8, and nearly every combination of them is exact.

This is a **sparse, content-specific** failure: two (width, height, content)
triples out of 51, with no geometric predicate separating them from the 49 that
pass. Any mechanism proposed for it must explain why *these two encodings* and no
other in the grid.

## 6. Gates

Run after the label fix: `the_pinned_420_oddheight_witness_is_pinned_and_decodes_byte_exact`,
`the_counting_oracle_diff_detects_one_flipped_oracle_byte`,
`a_pinned_444_inter_stream_chroma_units_inherit_their_own_quadrants_tx_type`,
`a_pinned_444_rect_inter_stream_resolves_each_chroma_unit_from_its_own_luma_leaf`,
`a_444_rect_strip_chroma_tiled_2x1_and_1x2_units_take_their_own_above_right_reach`,
`chroma_units*` — **6 passed, 0 failed**.

**No new gate committed, and deliberately so.** There is no decoder change to
pin. Committing a pixel gate for the label fix would be testing the test; and a
ratchet for `320x236` would be pinning an unexplained defect, which is what
lane-av1422anom's ratchet was and what I have spent three lanes arguing against.

## 7. `not_done`

* **The two cells are not fixed and are not explained.** Named state: entropic
  fork at coefficient read 6 of frame 0, inside a V-plane 8x8 unit's
  sign/Golomb tail, with the walk, the partition values, the scan, the
  placement, `all_zero`, `eob` and every base ctx/level all verified equal to
  aomdec.
* **The other 16-level partition rung pair (decode.rs:52836, the rect reader) was
  not audited** for the same label defect. It multiplies by `SUB_MI` already, so
  it is *not* the same bug, but I did not check whether it is in the same space
  as the 32-level rung at 52692 beside it.
* **No gate** (§6).
* **4:2:2 not re-measured** (needs `EC_AV1_ALLOW_422_PROBE`).
* **Full suite not run** (lane rules: scoped only).
