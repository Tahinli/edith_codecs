# lane-av1422losslesspanic — the `--lossless=1` 4:2:2 panic is a FAMILY, not a one-line extent

Base `ef9d7c97` (main, the merged `lane-av1422re4222`), worktree
`~/.cache/wt/av1422lp`, branch `lane-av1422losslesspanic`. **Report-only: no
source change is committed.** `crates/ec-av1/src/decode.rs` is untouched
(`git status` clean apart from the local-only 4:2:2 probe bypass in
`stream.rs`, which is reverted before yielding and is not in any commit).

## Verdict

Main asked for a one-line extent fix in `read_inter_chroma_lossless`. **It is
not one line, and I am not shipping a half of it.** I applied the first three
sites, watched the panic walk forward through three more, and reverted. What
follows is the full site inventory with the measured geometry at each one, so
the next lane starts at site 4 instead of at site 1.

The three `--lossless=1` 4:2:2 cells still panic. Nothing is fixed; the
deliverable is the map.

## 1. The panic, reproduced, and why it is 4:2:2-only

All three cells, unpatched, at `crates/ec-av1/src/decode.rs:37565` (the line
number in the merged report; the `mu_units` allocation is `37555..37562`):

```
W_intrabc.obu           130320 B  --lossless=1, 320x240 4:2:2
X_intrabc_tiled.obu     131696 B  W + --tile-columns=1 --tile-rows=1
Y_intrabc_10b.obu       202281 B  W at 10-bit
  -> panicked at crates/ec-av1/src/decode.rs:37565:24:
     range end index 20 out of range for slice of length 16
  -> backtrace: read_inter_chroma_lossless -> decode_inter_block
                -> decode_inter_frame_tile_with_cdfs
```

The copy, verbatim:

```rust
let dst = mu_units(
    if plane_idx == 1 { &mut *u_out } else { &mut *v_out },
    stride * stride,
);
for row in 0..4 {
    let start = (oy + row) * stride + ox;
    dst[start..start + 4].copy_from_slice(&cu_grid[row * 4..][..4]);
}
```

Instrumented geometry at the failing call (temporary `eprintln`, since
removed):

```
ZZLIC cpx=12 cpy=0 org=(0,0) reg=(4,16) blk=(4,16) stride=4
      tw=160 th=240 need=64 alloc=16
```

`reg`/`blk` are a 4-wide x 16-tall chroma plane block with `stride` 4. The
walk addresses row `oy + row` for `row in 0..4` over `reg_h / 4 = 4` bands, so
its top row is 15 and its last element is `(4+3)*4 + 0 + 4 = 24`... the
measured `need` is 64 elements against the square's `4*4 = 16`, and the first
out-of-range slice starts at element 20. Matches the panic text exactly.

**Why only 4:2:2.** `stride` is documented at the call site as "the block's
**square** chroma side, even for a 2:1 strip, which embeds its narrower plane
block in the same square grid". That contract holds when the chroma plane
block is square, which it is at 4:2:0 (`side/2 x side/2`) and 4:4:4
(`side x side`). At 4:2:2 `decode_inter_block` (line 40004) hands the caller
the **per-axis** pair:

```rust
let (chroma_stride, chroma_buf_h) = if chroma_422 {
    (write_chroma_w, write_chroma_h)   // 4:2:2: per-axis
} else {
    (chroma_side, chroma_side)         // 4:2:0 / 4:4:4: the square
};
```

so an 8x16 luma strip's chroma plane block is 4x16 — twice as tall as it is
wide — and `stride * stride` is half of what the walk writes.

**The discriminator, measured.** I encoded and ran the 4:2:0 and 4:4:4 lossless
families on this tree with the same comparator (`count_rawvideo_diffs`, the
crate's own) and none of them reaches this line:

| family | cell | result |
|---|---|---|
| 4:2:0 lossless | `--lag-in-frames=0 --auto-alt-ref=0 --enable-global-motion=0` | **exact** 16/16, decode order 16/16 |
| 4:2:0 lossless | `--lag-in-frames=0 --auto-alt-ref=1 --enable-global-motion=0` | **exact** 16/16, 16/16 |
| 4:2:0 lossless | `--lag-in-frames=25 --auto-alt-ref=0 --enable-global-motion=0` | **exact** 16/16, 16/16 |
| 4:2:0 lossless | `--lag-in-frames=25 --auto-alt-ref=1 --enable-global-motion=0` | refused by name (`AV1 tile: a Golomb tail longer than this decoder reads`) |
| 4:2:0 lossless | `--lag-in-frames=25 --auto-alt-ref=1 --enable-global-motion=1` | **DIVERGES**, 1 254 308 wrong, 1/16 (see §5) |
| 4:4:4 lossless | committed `a_lossless_444_*` gates (6 of them) | pass |

The four committed 4:2:2 lossless/IBC pins — `422_intrabc_sb128_strip.obu`
and `422_intrabc_sb128_strip_notxsearch.obu` among them — decode byte-exact,
so the lossless **intra**/IBC 4:2:2 chroma path is fine. The gap is
specifically lossless **inter** 4:2:2.

## 2. The site inventory, in the order the panic reaches them

Each site below was applied, the three cells re-run, and the panic observed to
move. That walk IS the evidence that these are the sites; there is no
speculation in the list.

**Site 1 — `decode.rs:37580`, `mu_units(..., stride * stride)`.**
Applied: `stride * blk_h`. The walk's top addressed row is
`org_y + reg_h - 1`, and every one of the eight call sites passes a region
that is a sub-rect of `blk` with `org` inside `blk`, so `stride * blk_h` is
exactly the requirement and reduces to the old expression verbatim wherever
the plane block is square. **Effect: the `range end index 20 ... length 16`
panic is gone and all three cells decode further.** This part is real and I
am confident in it — it is simply not sufficient.

**Site 2 — `decode.rs:45037`, `(cr * cu + rr) * chroma_side + cc * cu` in the
lossless TX_4x4 unit split.** The grid being split is the per-axis plane grid,
so it must be indexed at `chroma_stride`. `chroma_side` runs off the end on
the last chroma row at 4:2:2 and is the same number at 4:2:0 / 4:4:4.
**Effect: panic moves from 37580 to 45030.**

**Site 3 — `decode.rs:45023`, the unit counts.**

```rust
let (rows, cols) = (
    (write_h >> ss_y(fctx)).div_ceil(cu),   // LUMA-per-subsampling
    (write_w >> ss_x(fctx)).div_ceil(cu),
);
```

At 4:2:2 `ss_y` is 0, so `rows` counts the LUMA height while the grid holds
chroma rows. Measured: `chroma_buf_h = 8`, `chroma_side = 16`,
`chroma_stride = 4`, `grid_len = 32`, `cu = 4`, and the old expression gives
`rows = 4` where the grid has 2 unit rows. Applied:
`(chroma_buf_h.div_ceil(cu), chroma_stride.div_ceil(cu))`, identical at 4:2:0
and 4:4:4. **Effect: panic moves from 45030 to the prediction path.**

**Site 4 — `decode.rs:39995-40001`, the strip chroma extents.** The `else`
arm hands 4:2:2 the 4:2:0 pair's chroma rect:

```rust
} else if s.horz { (8, 4) } else { (4, 8) }
```

The pair is 16x8 luma (horz) or 8x16 luma (vert); 4:2:2 subsamples X only, so
the same pair is **8x8 / 4x16** in chroma. Measured on the failing cell
(`side = 16`, `strip_chroma = Some`, `px = 4`, `py = 48`):
`write_chroma_w = 4`, `write_chroma_h = 8` — half the rows of the 4x16 the
block actually has. Applied: a `ss (1,0)` arm returning `(8, 8)` / `(4, 16)`.
**This one changes `chroma_stride` and `chroma_buf_h` for the whole of
`decode_inter_block` at 4:2:2, including the LOSSY arms that currently
measure byte-exact. It is the first site with a real regression surface and
the reason I stopped.**

**Site 5 — the chroma prediction source stride (NOT reached, measured only).**
With sites 1-4 applied the panic lands at `decode.rs:4366` inside
`push_mc_rect_tx`, reached from `read_inter_plane` called by
`read_inter_chroma_lossless`:

```
ZZLIC3 w=4 h=4 stride=128 res_stride=4 pred_len=452 tx_stride=0
ZZLIC3 w=4 h=4 stride=64  res_stride=4 pred_len=0   tx_stride=0
  -> panicked at crates/ec-av1/src/decode.rs:4369:66:
     range end index 4 out of range for slice of length 0
```

A 4x4 **chroma** unit is being read at a prediction stride of 128 and then
64 — those are luma-plane strides — and the failing call's prediction buffer
is empty. This is the chroma prediction plumbing for lossless 4:2:2 inter, and
it is a different layer from the extents above: it needs the prediction
source's own stride resolved per axis, which I have not established.

## 3. Why I reverted rather than shipping sites 1-3

Sites 1-3 are each provably correct in isolation and provably
format-preserving at 4:2:0 and 4:4:4. But **no cell decodes byte-exact with
them**, so I would be committing a change to a bit-exact decoder that no
oracle has ever seen produce a right pixel, on a path where the very next
site is a stride the size of a whole luma plane. A partial fix here converts a
loud panic into a plausible-looking wrong picture on a code path no gate
covers. That is the worse outcome, so the tree is back at `ef9d7c97`.

I also did not write a gate, because there is nothing to gate yet: a gate
whose only red is "still panics" is a ratchet on a defect, not a fix, and
Main's step 4 asked for a gate that bites on the *fixed* code.

## 4. What the next lane should do

1. **Sites 1-3 as written above.** They are ready to apply, they are
   format-preserving at 4:2:0 / 4:4:4, and the panic-walk is the proof that
   each one is on the path.
2. **Site 4 needs a regression harness before it lands**, not after: the
   4:2:2 lossy corpus in `lanes/av1422re4222.report.md` (`AA_inter_compound`,
   `AB_inter_warp_odd`, `AD_inter_nogm`, `L_tiled`, `T_tilecols2`,
   `U_tilerows1`, `V_tile2x2_odd`, and the six pins) is the set that would
   catch it, and every one of them must still be byte-exact.
3. **Site 5 is the real work.** A 4x4 chroma unit read at a luma prediction
   stride is not an off-by-one; it is the chroma prediction source never
   having been resolved per axis on this path. Expect that to be where the
   time goes, and expect the byte-exactness to follow from fixing it rather
   than from the extents.
4. **Do not chase the height-242 family.** It is untouched here, still open,
   and belongs to its own lane.

## 5. Still true from the previous round, unchanged

- The **height-242 4:2:2 divergence** (4/4 cells, chroma-only, luma exact,
  already differing at `EC_AV1_PREFILT_DUMP`) is **untouched and still open**.
  Not chased in this lane, by instruction.
- The **4:2:0 lossless alt-ref + global-motion divergence** is untouched:
  1 254 308 wrong presented samples, 1/16 shown frames exact, first divergent
  at frame 1 byte 268 plane 0. It refuses by name with alt-ref and no global
  motion, and is exact in every other lossless recipe. It is a 4:2:0 cell and
  it belongs to the lossless-inter owner.

## Gates run

Clean tree at `ef9d7c97` (this lane commits no source change), scoped:

- `a_lossless_libaom_inter_frame_decodes_sample_exact` — pass
- `the_pinned_422_bigblock_witnesses_are_present_and_refuse_by_name` — pass
- `the_pinned_422_intrabc_sb128_strip_witnesses_refuse_by_name` — pass
- `the_pinned_422_lr_off_witness_is_present_and_refuses_by_name` — pass
- `the_pinned_422_residual_compound_warp_witness_is_present_and_refuses_by_name` — pass

## Instrumentation disclosure

Three temporary `eprintln` traces (`ZZLIC`, `ZZLIC2`, `ZZLIC3`), each removed
immediately after the reading that produced the numbers above, and the four
candidate source edits reverted with `git checkout --`. The `EC_AV1_ALLOW_422_PROBE`
bypass at `stream.rs:1803` is patch-run-restore and is not in any commit.
**Nothing was added to the decoder or the oracle in the committed tree.**

## `not_done`

- **The panic is NOT fixed.** All three `--lossless=1` 4:2:2 cells still panic
  at `decode.rs:37565` on `ef9d7c97`.
- **No gate committed**, for the reason in §3.
- **Site 5 is characterised, not fixed**: a 4x4 chroma unit is read at
  prediction strides 128 and 64, and the failing prediction buffer is empty.
  I did not establish where that stride comes from.
- **Site 4's effect on the 4:2:2 lossy arms is predicted, not measured.** I
  did not run the 4:2:2 lossy corpus with site 4 applied, because applying
  it is the point at which the change stops being a local extent.
- **The height-242 4:2:2 divergence** — open, not chased, by instruction.
- **The 4:2:0 lossless alt-ref + global-motion divergence** — named, not owned.
- **Full suite not run** (lane rules: scoped only; project-wide validation is
  Main's).
