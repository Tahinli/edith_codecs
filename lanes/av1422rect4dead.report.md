# lane/av1422rect4dead — `decode_rect4_16_intrabc` `else` arm at ss (1,0)

Branch of main `c38dd788ec4fc12e0b5d616f6c9773d7f849bb89`. Lane commit
`3a7f31c9`. Primary stayed on `main` throughout; nothing pushed, nothing merged.

## Verdict

UNREACHABLE at ss (1,0) — closed with an assertion, per the repo rule. The
arm is LIVE at ss (1,1) and inert at ss (0,0); behaviour at both is unchanged
(no decode-path edit, no halving rewrite).

## The arm (re-read on main, unchanged in the worktree)

`decode.rs` `decode_rect4_16_intrabc`, the `else` of the `(cw, ch)` select
(was 18500–18504) and the origin select (was 18516–18524):

- `!own_chroma` ⇒ `(pw / 2, ph / 2)` with `(pw, ph)` = (16,8)/(8,16) → (8,4)/(4,8)
- `!own_chroma` ⇒ origin `(pair_mi.1 * MI / 2, pair_mi.0 * MI / 2)`
- `own_chroma = own444 || own422`, `own422 = ss_x==1 && ss_y==0 && horz`

So at ss (1,0) the arm runs only with `!horz`, i.e. the (4,16) VERT_4 strip.

## Reachability, re-derived from the guard's own predicate (not inherited)

1. Sole caller: `decode_rect4_16_intrabc(` appears exactly once as a call in
   the decode body (`decode_rect4_16_strip`, 19100 on main). Crate-wide grep
   over `crates/**.rs`: only that site plus a doc mention in stream.rs.
2. `decode_rect4_16_strip(` has exactly two callers: `decode_rect4_16`
   (17746) and `decode_intra_rect_in_inter` (13951).
3. `decode_rect4_16` is dispatched only from the key-frame 16-level 1:4 arm
   (37569), inside `if part16 == PARTITION_HORZ_4 || part16 == PARTITION_VERT_4`
   (37555), BELOW `refuse_invalid_subsize((16, 16), part16)` (37248).
4. `decode_intra_rect_in_inter` reaches `decode_rect4_16_strip` only through
   `strip16` (46277), built from `fctx.inter_strip_chroma` (42346) whose ONLY
   writer is the inter 16-level 1:4 arm (56021), inside
   `part16 == PARTITION_HORZ_4 || part16 == PARTITION_VERT_4` (55939), BELOW
   `refuse_invalid_subsize((16, 16), part16)` (55705).
5. The guard's own predicate: `partition_subsize_dims((16,16), PARTITION_VERT_4)
   == (4,16)` and `(4,16) ∈ PLANE_BLOCK_INVALID_422` ⇒
   `chroma_plane_block_codable(4, 16, 1, 0) == false` ⇒ the guard errs before
   either dispatch. HORZ_4 gives (16,4), legal at (1,0) — and that strip takes
   `own422`, never the else arm.

Per-format disposition:

- ss (1,0): the only shape that could enter the arm is refused one level up;
  the arm is dead. The dispatch itself can never be reached with VERT_4.
- ss (1,1): both 1:4 subsizes are codable; the arm is live and correct (4:2:0
  pair geometry).
- ss (0,0): `own444` wins the select; the arm is unreachable by construction.

## Change

- `decode_rect4_16_intrabc` `else` arm: added
  `assert!(!(ss_x(fctx) == 1 && ss_y(fctx) == 0), ...)` naming the (4,16)
  VERT_4 shape and the ss (1,0) subsampling, above the unchanged
  `(pw / 2, ph / 2)` halving. Fires only if the closure breaks; cannot fire
  at ss (1,1) (condition false) or ss (0,0) (`own444` selected first).
- New gate `decode::tests::decode_rect4_16_intrabc_else_arm_stays_closed_at_422`:
  (a) the assertion must exist inside the function's `(cw, ch)` else arm and
  name `ss_x(fctx) == 1 && ss_y(fctx) == 0`; (b) caller counts re-derived
  (1 and 2); (c) both routes' `refuse_invalid_subsize((16, 16)` ordered above
  their 1:4 arm tests, scoped per enclosing function; (d) the closed cell's
  arithmetic re-executed — (4,16) invalid at (1,0), (16,4) codable at (1,0),
  both codable at (1,1) and (0,0).

`decode_intrabc_owned_rect` untouched (other lane's function).

## Non-vacuity (mutation-proof, red-before)

| Mutation | Gate |
|---|---|
| M1: delete the in-arm assertion | FAILED (expected) |
| M2: delete key-frame 16-level guard (37248) | FAILED (expected) |
| M3: delete inter 16-level guard (55705) | FAILED (expected) |

Worktree restored clean after each; committed tree is the unmuted one.

## Checks

- `cargo check -p ec-av1`: clean (one pre-existing `pin_reporting` dead-code
  warning, present on main).
- Scoped test: gate PASSES (1 passed). Pre-existing census test
  `every_chroma_walking_leaf_dispatch_is_guarded_or_closed_with_a_reason`
  still PASSES. No full suite per charter.

## Item 5 disposition

`lanes/av1floorshift.report.md` item 5: CLOSED by construction (assertion),
not merely by the guard one level up — the guard chain is now checked by the
gate, and the arm self-reports if it is ever entered at ss (1,0).
