# lane/av1sitecensus — W1-6: two subsize guards claimed by LEVEL, not by SITE

Base `5ba7a123` (`main`). Worktree `/home/tahinli/.cache/wt/av1sitecensus`, branch
`lane/av1sitecensus`. No push, no merge. Commit is one file:
`crates/ec-av1/src/decode.rs`, **+128 / -0** — the whole change is the new
section; no decode-path line is touched.

## The gap

`lanes/av1subsizesweep.report.md` §"Not closed" #2 named two guards the walk
census could not see:

> **Two guards are level-claimed, not site-claimed** (kf 8x8 second
> resolution, inter 128-root). The merged scan's count cross-check is what
> reds them.

Both are real, and the mechanism is the same in each case: a
`refuse_invalid_subsize((L, L))` exists **somewhere** in the guard's function,
and that is what satisfies the row — so deleting the guard the row is actually
about leaves a different guard standing in.

| site | guard | why the census was satisfied without it |
|---|---|---|
| `decode.rs:37621`, `decode_key_frame_tile_with_cdfs` | `refuse_invalid_subsize((8, 8), part8, fctx)?` — the **second** key-frame 8x8 resolution | the walk census's `decode_leaf_rect8` row is `AtResolution(KF, 8)`, which `find()`s the first `(8, 8)` guard in the function — `37322`. Two sites, one claim. |
| `decode.rs:54495`, `decode_inter_frame_tile_with_cdfs` | `refuse_invalid_subsize((128, 128), part128, fctx)?` | the `decode_block_128rect` row is `AtResolutionVia("read_sb128_root", 128)`; the inter decoder never routes through `read_sb128_root`, so its own 128-root guard is claimed by **no** row. |

Both guards matter: the KF one feeds `decode_leaf_rect8` on a raw `part8`
whose VERT shape is 4x8, BLOCK_INVALID at 4:2:2; the inter one is the only
thing above `match part128`, whose `half128_inter!` arms descend into 128x64 /
64x128 rects (also BLOCK_INVALID at 4:2:2).

## The change

New section **(g)** at the end of
`every_chroma_walking_leaf_dispatch_is_guarded_or_closed_with_a_reason`. Each
row claims a guard **by site**, using an `anchor` that is unique in the decode
body — the code the site hangs off — rather than `find()` from the body start
or the function start:

| row | anchor (unique in decode body) | level | dispatch it must precede |
|---|---|---|---|
| the key-frame 8x8 SECOND resolution | `let pre8 = dec.debug_state().0;` | 8 | `decode_leaf_rect8(` |
| the inter decoder's own 128-root guard | `macro_rules! half128_inter` | 128 | `half128_inter!((br_mi, base_mi.1), 128, 64,` |

The check, per row, all inside the guard's own function:

1. the anchor occurs **exactly once** in the decode body (else the row has
   stopped naming one site and says so);
2. the shape(s) the site walks are BLOCK_INVALID at some selectable
   subsampling mode — same `(1,0) (1,1) (0,0)` filter as the walk census,
   `(0,1)` excluded on libaom's own `color_config` authority;
3. the anchor is inside the named function (scope staleness reds);
4. `refuse_invalid_subsize((L, L))` exists **at or after the anchor** —
   this is the bite: a level guard elsewhere in the function cannot stand in;
5. within 40 lines of the anchor, so the match is *this* site and not a
   different one further down;
6. the dispatch token appears **after** that guard — a guard below the
   dispatch reds on ordering.

Search starts at the anchor, not at the function top: that is the whole fix,
and it is why the already-fixed `find()`-from-body-start defect in this file
does not recur here.

## Proof that each row bites

Each guard deleted individually, test run, guard restored.

**Delete `refuse_invalid_subsize((8, 8), part8, fctx)?` at 37621:**

```
the key-frame 8x8 SECOND resolution: `decode_key_frame_tile_with_cdfs` has NO
refuse_invalid_subsize((8, 8) at or after the site anchor
"let pre8 = dec.debug_state().0;" -- this site's guard is gone. The level
claim elsewhere in this test is still satisfied by the OTHER 8x8 guard in the
file, which is precisely why this row exists
```

```
test decode::tests::every_chroma_walking_leaf_dispatch_is_guarded_or_closed_with_a_reason ... FAILED
```

**Delete `refuse_invalid_subsize((128, 128), part128, fctx)?` at 54495:**

```
the inter decoder's own 128-root guard: `decode_inter_frame_tile_with_cdfs`
has NO refuse_invalid_subsize((128, 128) at or after the site anchor
"macro_rules! half128_inter" -- this site's guard is gone. The level claim
elsewhere in this test is still satisfied by the OTHER 128x128 guard in the
file, which is precisely why this row exists
```

```
test decode::tests::every_chroma_walking_leaf_dispatch_is_guarded_or_closed_with_a_reason ... FAILED
```

**Both restored.** `git diff --stat` is `1 file changed, 128 insertions(+)` —
identical to the pre-mutation state, so both restores were byte-exact and no
decode-path line drifted.

## Verification

| check | result |
|---|---|
| `cargo check -p ec-av1 --tests` | clean |
| `every_chroma_walking_leaf_dispatch_is_guarded_or_closed_with_a_reason` | 1 passed, 0 failed |
| `every_partition_symbol_resolution_is_guarded_before_it_dispatches` (sibling structural scan) | 1 passed — the 18-guard count cross-check is untouched |
| mutation 1 (delete kf 37621) | RED, names the site |
| mutation 2 (delete inter 54495) | RED, names the site |

**Commit-hook reflow.** The commit hook ran rustfmt over the whole file, so the
committed `decode.rs` diff is `134 insertions / 4 deletions`, not `+128 / -0`.
The 4 deleted lines are two copies of one pre-existing two-line closure
(`let narrow = |p: &PlaneBuf<'_>| -> Vec<u8> { ... }`) reflowed onto a single
line by the formatter — a whole-file reflow of pre-existing code, not a
semantic edit and not mine. My own addition is the +128, and the census test is
green at the committed tip, re-run after the hook.

Local scoped runs only; no full `ec-av1` suite (the VPS fleet owns those).

## Scope note, stated plainly

Both rows are **existence + ordering** claims at a named site. They do not
close `lanes/av1subsizesweep.report.md` §"Not closed" #1 — the census is still a
derived site list, so a walk that is neither named here nor reachable through a
`partition_w<N>` read is still invisible. This closes #2 only. No other census
row was changed; the decode path was read, not edited.

## Housekeeping

An early edit round in this lane was issued with a worktree-relative path from
the primary checkout's cwd and leaked both the section and a mutation into the
primary `crates/ec-av1/src/decode.rs`. Caught by `git status` before any commit
and reverted with `git checkout --` on that one file; the primary is clean at
`5ba7a123`. Every edit after that used absolute paths under the worktree. No
commit on this branch contains the leaked hunk.