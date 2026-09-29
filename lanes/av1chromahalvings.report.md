
---

# r5 — rebased onto `3b691e13`. Item (3) is ALREADY DONE by another lane; item (2) confirmed still deferred; item (1) NOT STARTED.

## r5.0 Two of my three chartered items changed status on the rebase, and both changes are retractions in my favour

**Item (3), `suppress_internal_lf_edges` (`16758`) — ALREADY FIXED, by Onur-4's C1.**
On `3b691e13` the function now reads:

```rust
fn suppress_internal_lf_edges(&mut self, at_mi: (usize, usize), w_mi: usize, h_mi: usize, fctx: &FrameCtx) {
    let (uv_w, uv_h) = (((w_mi * MI) >> ss_x(fctx)).max(4) as u8, ((h_mi * MI) >> ss_y(fctx)).max(4) as u8);
```

`fctx` threaded in and the halving replaced by `>> ss_*` — exactly the fix I
chartered, landed by another lane. **My r1/r2 report's claim that this site is
"format-blind today, a structural constraint" is obsolete**: it was true of
`f33b9d41` and is false of current main. Retracted.

**Item (2), `cfl_ac_q3_at` — still format-blind, re-measured on the rebased tree.**
The signature on `3b691e13` is still `fn cfl_ac_q3_at(px, py, bw, bh, sample:
impl Fn(usize, usize) -> i32) -> Vec<i32>` with `let (cw, ch) = (bw / 2, bh / 2);`
immediately after. No `fctx`, so the subsampling does not exist inside the
function and cannot be derived there. **It stays deferred, and now with a
measured reason rather than an assumption**: the structural constraint is real
and unchanged, and the witness is real (188 rung hits on the 4:4:4 set), so
what is missing is only the threading through the CfL call tree plus a
CfL-specific pixel gate. Line numbers post-rebase: definition at `19950`,
halving at `19957`.

## r5.1 The `decode_intra_sub8_leaf` 4:2:2 gap (recorded, not implemented)

`decode_intra_sub8_leaf` at `decode.rs:45340` (body `45340..46168`) takes
`has_chroma: bool` and branches its chroma handling on that alone. The one
chroma comment inside it reads:

> `lane-av1llintercdf). At 4:2:0 a sub-8 shape's chroma block is ...`

There is **no `chroma_444` branch and no `chroma_422` branch** — the sub-8 leaf
has a single chroma path, so at 4:4:4 it computes whatever the 4:2:0 geometry
gives it, and 4:2:2 has no path at all (it is refused by name at the sequence
header, so that is safe, but the shape is not modelled).

**Caller set** (two call sites, both passing `has_chroma`): `decode.rs:44470`
and `decode.rs:46356`.

**Why it is a named gap and not a fix:** the 4:2:2 lift needs this shape, but
implementing it is a second `TxbSet` family plus a sub-8 chroma geometry
decision, and this lane has no 4:2:2 witness (the decoder refuses 4:2:2 by
name at the sequence header, so nothing in the pin set exercises it). Recorded
for whoever does the lift; not implemented here.

## r5.2 Item (1) — the TxbSet — NOT STARTED, and I am not going to fake it

I rebased, re-measured both other items, and recorded the sub-8 gap. I did
**not** add `ChromaRect32x64`/`ChromaRect64x32`, and I did **not** land the
ss-derived extent. My budget ran out mid-lane and the honest state is:

- witness `fixtures/r512.obu`: COMMITTED on this branch;
- red-before: MEASURED (r4.2) and quoted in r5.4;
- blocker: NAMED, and the refusal string is exact;
- fix: NOT DONE.

The remaining work is a real piece of engineering, not a swap: a CDF set per
shape, its `rect_scan`, and the rect forms of `base_ctx_rect`/`br_ctx_rect`,
following `lane-inter4`'s precedent for the rect inter sets. Shipping a
partial version — a set that decodes but whose contexts I have not verified
against the oracle — would put a silent wrongness into the tree, which is the
one outcome this whole lane has been removing.

**Unblock, unchanged and still ordered:** (1) `ChromaRect32x64` and
`ChromaRect64x32`; (2) then the extent + the eight chroma MV origin pairs, with
r4.2 as the red-before, and the 4:2:0 and 4:4:4-lossless arms re-measured
byte-identical (arithmetic-first: at `ss = 1`, `>> 1` IS `/ 2`).

## r5.3 The stale-binary tell, written down as instructed

**A green gate result alongside a target that does not compile is a stale
binary, and `git checkout <branch> -- <file>` is what produces it.** It bit me
twice in one turn: a `git checkout main -- stream.rs` pulled a newer
`stream.rs` (referencing `intra_128_in_inter_mu_chroma_hits`, absent from my
base's `decode.rs`) into a branch based on an older commit; the test target
stopped compiling; and the gates kept reporting green from the old binary. The
only reason I caught it was that a `--no-run` build printed errors while the
gate run said `ok`. **Treat any `--no-run` error that coincides with green
gates as invalidating every gate result in that turn, and re-measure.**

## r5.4 The red-before, quoted verbatim (the chroma-only signature is the evidence)

```text
frame 0 (key)   Y=166229 wrong from 65728   U=155383 from 32848   V=153498 from 32848   total 475110
frame 1 (inter) Y=EXACT                      U=74708  from 49282   V=73501  from 49280    total 148209
frame 2 (inter) Y=EXACT                      U=9216   from 114880  V=8192   from 229760   total  17408
```

Luma byte-exact on both inter frames while chroma is wrong is what identifies
this as the chroma-plane sizing defect and not a block-geometry one.
