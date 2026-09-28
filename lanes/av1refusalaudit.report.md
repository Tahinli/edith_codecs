# lane-av1-refusalaudit — the four witness2-proven-unreachable refusals, dispositioned

Base: `main` **fe3e8418** (2026-09-26). Worktree `edith_codecs-av1refusalaudit`,
branch `lane-av1-refusalaudit`,
`CARGO_TARGET_DIR=$HOME/.cache/cargo-target-av1refusalaudit` (private). No push.
Region: `crates/ec-av1/src/refusal_inventory.rs` ONLY — no decoder behaviour
change, no refusal string added or removed.

## TL;DR

The four Wave-2 census refusals that the lane-av1-witness2 hunt
(`../edith_codecs-av1witness2/lanes/av1witness2.report.md`, committed on
`lane-av1-witness2`; verdict **0 / 4 guards reached** in 31 valid encodes) are
now dispositioned PROVEN/unreachable in the inventory, each citing the hunt:

| # | Refusal | Disposition |
|---|---|---|
| 1 | `a 64-axis strip whose chroma unit has no coefficient table` | **NEW PROVEN pair** + new ENUMERATION test (§1) |
| 2 | `a rectangular inter luma transform unit whose shape has no coefficient table set here` | already PROVEN (`every_rect_transform_shape_the_census_lists_has_a_coefficient_table_and_scan`); annotation extended with the hunt (§2) |
| 3 | `a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding` | already PROVEN (`a_block_shape_census_over_three_real_streams_leaves_the_rect_residual_refusal_unreachable`); annotation extended with the hunt (§3) |
| 4 | `a frame whose segmentation enables SEG_LVL_REF_FRAME/SKIP/GLOBALMV (…)` | already PROVEN (`a_frame_whose_segmentation_overrides_a_block_mode_is_refused_by_name`); annotation extended with the hunt (§4) |

Untouched, per charter: every refusal with a live witness, and the reserved
4:2:0 group-tail chroma SKIP arm.

## §1 — the 64-axis strip chroma table guard (new PROVEN entry)

`decode.rs:15233`, the `_` arm of the chroma-unit `match` in
`decode_block_rect64` (reached only for depth-0, non-skip, non-lossless
superblock-level HORZ/VERT/1:4 strips). The witness2 hunt's static reduction:
the handled arms `(32,32) (32,16) (16,32) (32,8) (8,32)` cover every conformant
strip unit in both supported chroma formats — at 4:4:4 the strip set
64x32/32x64/64x16/16x64 tiles to (32,32)/(32,16)/(16,32); at 4:2:0 the halved
footprints land on (32,16)/(16,32)/(32,8)/(8,32). A 128-root half is
`decode_block_128rect`'s and the 128 root has no 1:4 arm. Dynamically: 8 real
aomenc key-frame encodes (screen+intrabc, rect+1:4, sb 64 and 128, cq 10..63,
cpu-used 0..2) never reached the arm; the three streams that decode to
completion never enter `decode_block_rect64` at all.

Encoded as the repo's shape-3 convention (keep the guard, add an ENUMERATION
test that derives the gate's own table): new test
`every_chroma_unit_a_64_axis_strip_can_present_has_a_coefficient_table` in
`refusal_inventory.rs`, which

1. derives the caller set from decode.rs itself
   (`read_sb128_root`, `decode_intra_rect_in_inter` — a 128-sided or sub-8
   caller would invalidate the domain),
2. pins the unit derivation spellings (`bw >> ss_x`, the `.min(32)` cap —
   asserted unique),
3. parses the five arms out of the `match` between the cap and the refusal
   string (class `table-and-reader-move-together`) and asserts they equal
   exactly the five handled units,
4. walks the strip domain (four 64-axis 2:1/1:4 strips × {4:4:4, 4:2:0} — the
   mixed formats are refused at the sequence header) through the mirrored
   derivation and asserts every combo lands on a handled arm, with
   non-vacuity asserted both ways (8 combos checked; every arm hit).

PROVEN pairing added:
`("a 64-axis strip whose chroma unit has no coefficient table",
"every_chroma_unit_a_64_axis_strip_can_present_has_a_coefficient_table")`.

## §2 — rect inter luma TU table guard

Hunt: 8 encodes (`s2a`..`s2h`), `--enable-tx-size-search=1`, rect+1:4 on,
`--min-partition-size=4`, 4:2:0 and 4:4:4, sb 64/128, cq 10..45, cpu-used
0..2 — **no refusal**, while the domain was genuinely exercised (whole-block
32x8 inter TUs decoded, sub-8 rect pieces decoded). The census enumeration's
fourteen rect shapes are the whole alphabet, so the `_` arm has no conformant
input. Annotation extended on the existing PROVEN entry.

## §3 — the rect residual (HORZ_B) guard

Hunt: 8 encodes (`s3a`..`s3h`), banded + busy content, inter,
`--max-partition-size=32`, single-ref variant, cq 20..45 — **no refusal**;
every rect strip the encoder produced sat inside `rect_inter_residual_supported`.
Static half (same hunt, source-read): the supported list plus the partition
walker's piece set (SB pieces 64x32/32x64/64x16/16x64, the 32- and 16-level
rect/1:4/AB twins, 128 halves tiled as two TX_64X64) enumerate every rect
footprint AV1 partitioning can present — 1:4 stops at 64 (the 128 root has no
1:4 symbol). Annotation extended on the existing PROVEN entry.

## §4 — the segmentation feature guard

Hunt: 8 encodes (`s4a`..`s4h`, 7 valid — aq-mode 4 is out of range for this
aomenc build): aq-mode 1/2/3, aq-mode 3 + `--delta-lf-mode=1`, 4:4:4, banded —
**no refusal**. This aomenc build has **no `--enable-segmentation` flag at
all**; aq-mode is the only segmentation driver. Segmentation genuinely enabled
in s4a/s4b (`seg=true` on 3/3 frames) and those streams decode clean — the
hunt re-confirms the PROVEN annotation's claim ("What aomenc codes is
SEG_LVL_ALT_Q tables and segment_id symbols, and nothing else"). Annotation
extended on the existing PROVEN entry.

## Verification

- `cargo check -p ec-av1 --all-targets` — **0 warnings**.
- Inventory self-checks green:
  `the_decode_path_refuses_exactly_the_listed_cases`,
  `every_proven_refusal_names_a_test_that_exists`,
  `capability_claims_are_declared_not_scattered`,
  `gates_that_swallow_a_decode_error_are_declared`, and the new
  `every_chroma_unit_a_64_axis_strip_can_present_has_a_coefficient_table` —
  5 passed, 0 failed.
- Inventory numerator: **33 refusals + 1 capability claims, 32 proven**
  (before this lane: 31 proven — the 64-axis strip chroma entry was the one
  unmeasured refusal of the four; nothing else moved).

No fixtures committed; no `/tmp` artefacts referenced by any gate added.
