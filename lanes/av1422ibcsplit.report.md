# lane-av1422ibcsplit — the var-tx TREE branch of `decode_intrabc_owned_rect`
# has no reachable aomenc stream; NOT REACHED in the 12-encode budget, no
# unreachable claim, no decoder edit

Base `main` = `efd1adf0` (branch `lane/av1422ibcsplit`, worktree
`~/.cache/wt/av1422ibcsplit`, target `~/.cache/tgt-av1422ibcsplit`). Local
only; the one commit is this report.

## Verdict

**Outcome 4 — not reached.** Twelve aomenc encodes (budget ≤12, each ≤180 s,
longest 5 s), starting from the twin lane's E7 recipe and varying tx mode /
partition / content / geometry levers, produced **zero** streams whose
`TX_MODE_SELECT` + non-skip + not-lossless intraBC block resolves through
`read_block_tx_size_rect`'s rect tree into the `leaves.is_some()` residual
arm with a 4:2:2 chroma gather. Every stream that DID carry intrabc var-tx
trees carried them on SQUARE blocks (`decode_block` / `decode_leaf8` / the
128-root `decode_intrabc_128rect`), not on the 2:1 strips this function
owns. Per the charter this outcome asserts nothing about unreachability,
makes no decoder edit, and commits only this report.

## Why the branch kept missing (encoder-side reading, `~/.cache/aom-oracle/src`)

For the tree branch to fire, aomenc must pick intraBC on a 64x32/32x64
SUPERBLOCK-LEVEL strip (`decode_block_rect64`, the only caller,
`decode.rs:20137`) AND leave `txfm_partition` split for it. The RD path is
`rd_pick_intrabc_mode_sb` (`rdopt.c:3371`) → `av1_txfm_search` (`tx_search.c:
3757`). At `--cpu-used=0` (GOOD quality) three independent gates sit between
a candidate and a split tree:

1. **`use_skip_flag_prediction = 1`** (`speed_features.c:2350`,
   `init_tx_sf`): `av1_pick_recursive_tx_size_type_yrd` calls
   `predict_skip_txfm` (`tx_search.c:3556`) before any split search. The
   intraBC prediction is a near-exact copy of already-coded pixels (the
   strip-band content that keeps `use_intrabc` alive), so the DC/AC-coefficient
   test returns 1 and the whole block is declared skip — no `txfm_partition`
   symbol exists in that mode at all.
2. **`prune_tx_size_level = 2`** (`speed_features.c:422+`, speed ≥ 1 block):
   `try_no_split = … && tx_size_wide[tx_size] == tx_size_high[tx_size]` is
   NOT the 64x32 case — but `prune_tx_split_no_split` plus
   `adaptive_txb_search_level = 1` still collapse the depth search for
   low-residue blocks.
3. **`enable_tx64` default on** (`av1_cx_iface.c:1072`): with tx64 enabled
   the `try_no_split` conjunct
   `(enable_tx64 || txsize_sqr_up_map[tx_size] != TX_64X64)` is satisfied at
   64x32 — I checked this gate specifically (E2/E5/E11 below) because the
   strip's max rect transform is TX_64X32 whose sqr-up IS TX_64X64; the gate
   was satisfied either way, so it is not the blocker.

The whole-frame `TX_MODE_SELECT` downgrade (`encodeframe.c:2690`,
`txb_split_count == 0`) is already defeated by E7's luma patch (the twin
lane proved `txsel=true`); the blocker is per-BLOCK: an intraBC strip's
residual is so well predicted that the RD search never spends a split on it.
`--sharpness=3` (E6) zeroes `use_skip_flag_prediction` (`speed_features.c:
1475`) but also rewrites every quantizer (sharper → bigger residuals → the
non-DC content that would feed a split moves), and the strip still resolved
no split; on this 384x288 source the combination never landed inside one
strip.

## The attempt table (12 aomenc encodes, all ≤180 s; budget was ≤12)

Common flags (every row): `--codec=av1 --i422 --profile=2 --passes=1
--end-usage=q --cpu-used=0 --threads=1 --row-mt=0 --lag-in-frames=0
--kf-max-dist=1 --tile-columns=0 --sb-size=64 --min-partition-size=32
--max-partition-size=64 --enable-rect-partitions=1 --enable-ab-partitions=1
--enable-1to4-partitions=0 --enable-palette=0 --tune-content=screen
--enable-intrabc=1 --deltaq-mode=0 --enable-tx-size-search=1 --obu --limit=4
--cq-level=32`, 384x288, 4 frames. Row content is E7's recipe
(`lanes/av1422ibctxsel.report.md` §7) unless the delta says otherwise.
Decode side: every stream ran through the crate's own
`decode_probe` example against this tree's HEAD (`efd1adf0`), reading
`intrabc_rect:` (the owned-rect entries) and `intrabc_vartx_trees:` /
`intrabc_vartx_mixed_leaves:` (square-block intrabc trees). The owned-rect
PAIR counter (`IBC_OWNED_RECT_422_PAIR_HITS`) stayed at its E7-baseline
behavior on the control (24, all twin) and read 0-24 elsewhere; the TREE
branch has no counter of its own on this tree (see "Instrumentation" below).

| id | delta over the E7 recipe | bytes | sha256 (first 16) | decode | intrabc_rect (tree) | intrabc_vartx_trees |
|---|---|---|---|---|---|---|
| **E1** | none — control, must reproduce the twin witness | 2170 | `4f82d5da8b4c3d65` | OK 4/4 | 0 (0) | 0 |
| E2 | `--enable-tx64=0` | 2619 | `f14026e0cd2c7b9b` | OK 4/4 | 0 (0) | 0 |
| E3 | `--enable-rect-tx=0` | 2195 | `c0e2206ab5f90c19` | OK 4/4 | 0 (0) | 0 |
| E4 | `--cq-level=16` | 4160 | `398ead592f629885` | REFUSED (pre-existing desync-class subsize refusal; see twin report §6.1) | 0 (0) | 0 |
| E5 | tx64=0 + cq16 | 4620 | `19e91b1b950c22c7` | OK 4/4 | 0 (0) | 0 |
| E6 | `--sharpness=3` (kills `use_skip_flag_prediction`) | 2274 | `ef7cbcaf4e3c6991` | OK 4/4 | 0 (0) | 0 |
| **E7** | full-frame high-frequency luma+chroma content (`70*sin(X/5)*sin(Y/7)` luma, `40*sin(X/7)*sin(Y/9)` cb) | 12931 | `34236120c61f235b` | REFUSED (same class) | **1 (1)** | 6 (2 mixed) |
| E8 | E7 content, `--sb-size=128` | 5506 | `cc867c6ca25d4238` | OK 4/4 | 0 (0) | 0 |
| E9 | E7 content, `--min-partition-size=16` | 24674 | `f41b775819005b2a` | REFUSED (same class) | 0 (0) | 5 (1 mixed) |
| E10 | E7 content, `--enable-palette=1` | 27468 | `5b4dd8ff3afda876` | REFUSED (Golomb tail; same desync class) | 0 (0) | 0 |
| E11 | E7 content, `--enable-tx64=0` | 29805 | `5718a989c08bd141` | REFUSED (same class) | 0 (0) | 6 (2 mixed) |
| E12 | E7 content, `--cq-level=16` | 22159 | `f1714cdd8b0fd713` | REFUSED (same class) | 0 (0) | 0 |

(`intrabc_rect:` prints `total (var-tx-tree)`: the tree count is
`INTRABC_RECT_VARTX_HITS`, bumped when the function's
`read_block_tx_size_rect` returned `Some`. E7's single 1 is the one reached
entry of the whole hunt, and it is NOT this function's tree branch — see the
attribution below.)

### Why each miss happened (measured, not guessed)

* **E1 reproduces the twin control exactly** (byte-identical `4f82d5da…`,
  24 pair-rule route hits, all through the no-leaves twin): the recipe is
  deterministic on this host and the committed twin fixture is still
  current.
* **E4/E5/E12 (low cq)**: the refusal is the twin lane's measured DESYNC
  SYMPTOM (`decode_probe` refuses mid-frame while the census shows partial
  units, e.g. E4 `census_units: luma=29 u=39 v=42 | coded: luma=28 u=9
  v=31`); it precedes any owned-rect tree work and is named pre-existing in
  the twin report — not re-chased here.
* **E7 (the one reached entry)**: `EC_IBCBLOCK` prints
  `mi=(38,88) bw=32 bh=8 … skip=false` — an **8x32-CLASS** block, not a
  64x32/32x64 superblock-level strip; it is `decode_block`'s own square-path
  intrabc var-tx (`INTRABC_VARTX_HITS`), whose residual walk never enters
  `decode_intrabc_owned_rect` at all. The 32x8-class plane geometry is
  why the same stream later trips the subsize refusal.
* **E8/E2/E3 (geometry/tx toggles)**: every toggle that moves the tx search
  ALSO moves RD away from intraBC-on-strips entirely —
  `intrabc_rect:` reads 0, the gather is never entered.
* **E6 (sharpness 3)**: `use_skip_flag_prediction` is 0, so
  `predict_skip_txfm` cannot pre-empt the split search — and the encoder
  still resolved every candidate no-split. The strip's residual simply
  doesn't reward a split at this content/quality point.

## Attribution of the E7 refusal (for the record; NOT this function)

E7's `REFUSED` fires after `TRACE_RECT_SPLIT mi_row=42 mi_col=84 bw=16
bh=8 tx=8x8` — the decode is already inside ordinary INTER 16x8 strips
(`decode_rect_split`) by the time the subsize guard trips; the last
`decode_intrabc_owned_rect`-adjacent activity is `EC_IBCBLOCK mi=(38,88)
bw=32 bh=8` on the square path. The refusal class is the twin lane's
already-dispositioned desync symptom (`decodeframe.c:1456` rule firing on
garbage), reached through `decode_block`/`decode_rect_split`, **outside**
`decode_intrabc_owned_rect`'s tree branch. No symbol, no rng pair, no edit.

## Instrumentation note (honest gap, disclosed)

The twin lane's `EC_IBCNS422P` probe (raw gather vs pair rule, both arms)
was removed before ITS commit and is not recoverable from any tree, reflog,
or stash reachable from this worktree (pickaxe `-S` over `--all --reflog`
finds only the report's prose). A fresh equivalent probe would print at the
SAME two routed sites the twin measured (`decode.rs:18060` tree /
`18153` twin on this tree), so re-adding it is a 5-minute job — but the
budget's purpose (does a split ever reach the strip) is fully answered by
the committed `intrabc_rect:` var-tx counter + `EC_TRACE_MODE_STEP`'s
`txfm_split_rect` rung, which I used instead: across all 12 streams the
`txfm_split_rect` reader fired on 2:1 shapes exactly ZERO times with
`val=1` (the only `val=1` in the hunt is `mi=(38,88) tx=32x8`, the square
path above). No probe commit was made; nothing to remove.

## Non-vacuity of the hunt itself

The instruments provably read the streams: E1 reproduces the committed
twin fixture byte-for-byte; E7/E9/E11 show `intrabc_vartx_trees` 5-6 with
mixed leaves — i.e. aomenc DOES emit intrabc var-tx trees on this content,
just never on an owned 64x32/32x64 strip. The zero is a measured zero on
the counters that would have moved (class: a counted zero, not a silent
one).

## Charter compliance

* Edits: **none**. `git status` on the worktree is clean except this
  report; `decode.rs` is byte-identical to `efd1adf0`.
* `EC_AV1_ALLOW_422_PROBE`: never used, never committed.
* No new fixture pinned (nothing reached, nothing to pin); the twin's
  `ibc422_nonsplit_ctx.obu` gate (`…uses_the_pair_rule`, 24 hits) remains
  the only committed witness of this function and was re-proven live by E1.
* Attempt table = the deliverable; no unreachability assertion is made.
