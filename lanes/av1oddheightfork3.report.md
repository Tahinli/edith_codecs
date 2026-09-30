# lane-av1oddheightfork3 — the 320x236 / 322x248 fork is CLOSED: a whole-block TXFM_CONTEXT publish was overwriting a var-tx tree's per-leaf sizes

Base `2005a35d` + the predecessor's instrument commit `b8af62fa`, branch
`lane-av1oddheightfork2`, worktree `~/.cache/wt/av1oddheightfork2`.

## 0. Headline

1. **Both cells are byte-exact now**, measured against live
   `aomdec --rawvideo`: `420_oddheight_320x236_diverging.obu` and the
   `322x248` cell of the same recipe go from `0/16` frames exact to `16/16`,
   `Y/U/V = 0/0/0` (§4). The red-before was reproduced on this tree by
   reverting the fix and matches the recorded numbers digit for digit.
2. **The named writer** is `Neighbours::fill_lf_grid_rect`'s intra-only
   `TXFM_CONTEXT` publish — `crates/ec-av1/src/decode.rs:9734` (was `:9680`)
   — reached from four block bodies. It wrote the BLOCK's own transform size
   over the whole footprint **after** the var-tx tree had already published
   each leaf's size. libaom never performs that write on the var-tx path
   (`parse_decode_block` is an either/or, `decodeframe.c:1227-1236`).
3. **The predecessor's hypothesis is REFUTED, with the numbers.** The
   suspect named in the ticket — "`txsize_to_bsize` for a rect `txb_size` is
   the awkward part" — is **not** the defect. `txfm_partition_update_rect`
   takes its extent from the NODE and its value from the leaf, which is
   exactly `txfm_partition_update`; the rect case is correct today, and a new
   test pins it (§6). The tile's own `EC_VARTXCTX` rung puts the fault on
   `left` alone: `above` agreed at 8 on both sides.
4. **No push, no merge, no re-encode of any committed pin.** The one pin
   that was not committed (`322x248`) was reproduced from the recipe
   `lanes/av1422anom.report.md:200-208` and matches the recorded sha256
   byte-for-byte (§4.2) — I did not add it as a fixture, see §8.

## 1. The measurement that closed it

The predecessor's §6 asked for one rung: the OPERANDS of
`txfm_partition_context`. It is now derived from the generator, not added by
hand — `scripts/instrument-aom-oracle.sh:1295-1351` (rung 18, the
`PYVX` block), which also re-derives the pre-existing hand-added `EC_VARTX`
print, so both are now script-owned and a rebuild from the script cannot drop
either (that loss is exactly what had happened: `EC_VARTX` was in the oracle
tree and in no script). Re-running the script is a no-op; the oracle rebuilds
with `ninja -C ~/.cache/aom-oracle/build aomdec`.

```
EC_VARTXCTX mi=(44,58) row=0 col=0 above=4 left=4 txw=8 txh=16 ctx=14
EC_VARTXCTX mi=(44,58) row=0 col=0 above=4 left=4 txw=8 txh=8  ctx=17
EC_VARTXCTX mi=(44,58) row=2 col=0 above=8 left=4 txw=8 txh=8  ctx=16
EC_VARTXCTX mi=(44,60) row=0 col=0 above=8 left=8 txw=8 txh=16 ctx=13   <- aomdec
EC_ISTEP  mi_row=44 mi_col=60 name=txfm_split_rect val=0 ctx=12 above=8 left=16 maxblk=16 tx=8x16  <- ours
```

**`above` agrees (8 = 8). `left` differs: 8 against 16.** So the answer is the
ticket's second shape: the block to the left published the wrong size — and
not because of its split children, but because something wrote the whole
block over them afterwards.

## 2. The writer, named, with libaom's line for comparison

**Site (ours):** `crates/ec-av1/src/decode.rs:9734`, inside
`fill_lf_grid_rect_inner` (the `set_txfm_ctxs` stand-in that runs when
`fctx.intra_only`), called as `neighbours.fill_lf_grid_rect(...)` from four
bodies that had just run a var-tx tree:

| body | reader it ran | former publish |
|---|---|---|
| `decode_intrabc_rect` | `read_block_tx_size_rect` (`:14192`) | `:14669` → now `:14727` |
| `decode_intrabc_128rect` | `read_block_tx_size_rect` (`:14751`) | `:15297` → now `:15357` |
| `decode_intrabc_owned_rect` | `read_block_tx_size_rect` (`:17439`) | `:17758` → now `:17820` |
| `decode_rect4_16_intrabc` | its own inline `read_var_tx_size` tree (`:18216`) | `:18436` → now `:18507` |

**Extent it used:** the BLOCK's own mi footprint, `w_mi * MI` above cells and
`h_mi * MI` left cells, carrying the block's transform size —
`txfm_partition_update_rect(self, at_mi, (tx_px, tx_h_px), (w_mi * MI, h_mi * MI))`.

**Extent libaom uses on that path:** none. `parse_decode_block`
(`decodeframe.c:1227-1236`) is an either/or —

```c
if (cm->features.tx_mode == TX_MODE_SELECT && block_signals_txsize(bsize) &&
    !mbmi->skip_txfm && inter_block_tx && !xd->lossless[mbmi->segment_id]) {
  ... read_tx_size_vartx(xd, mbmi, max_tx_size, 0, idy, idx, r);   /* per-leaf */
} else {
  mbmi->tx_size = read_tx_size(...);
  set_txfm_ctxs(mbmi->tx_size, xd->width, xd->height,
                mbmi->skip_txfm && is_inter_block(mbmi), xd);       /* once, whole block */
}
```

— and the var-tx branch's only writer is `read_tx_size_vartx`'s
`txfm_partition_update(above_ctx + blk_col, left_ctx + blk_row, tx_size, tx_size)`
per resolved leaf (`av1_common_int.h:1684-1695`), which writes the resolved
leaf's size over the extent of the node that leaf resolved.

**The measured damage.** The INTRABC 8x16 block at mi(44,58) split into two
8x8 leaves (aomdec's `ctx=17 s=0` at row 0 and `ctx=16 s=0` at row 2; ours
identical). The tree's two writes put `8` into `left_txfm[44..48]`; the
whole-block publish then put the block's `16` back over those same four cells.
The next block, the INTRABC 8x16 at mi(44,60), read
`txfm_partition_context` with `left = 16` where libaom has `8`, took ctx 12
against 13, decoded `is_split = 0` against `1`, and read no children against
libaom's two. `EC_TXUPD` on our side shows the two writes back to back:

```
EC_ISTEP mi_row=44 mi_col=58 name=txfm_split_rect val=1 ctx=14 above=4 left=4 maxblk=16 tx=8x16
EC_ISTEP mi_row=44 mi_col=58 name=txfm_split val=0 ctx=17 ; EC_TXUPD mi=(44,58) tx=(8,8) txb=(8,8)
EC_ISTEP mi_row=46 mi_col=58 name=txfm_split val=0 ctx=16 ; EC_TXUPD mi=(46,58) tx=(8,8) txb=(8,8)
EC_LFGRID mi_row=44 mi_col=58 w_mi=2 h_mi=4 tx_px=8 tx_h_px=16
EC_TXUPD mi=(44,58) tx=(8,16) txb=(8,16)      <-- the defect: the block's own size, over the leaves
EC_ISTEP mi_row=44 mi_col=60 name=txfm_split_rect val=0 ctx=12 above=8 left=16 ...
```

## 3. The fix

* `fill_lf_grid_rect` is split into a thin wrapper and
  `fill_lf_grid_rect_inner(.., publish_txfm_bands: bool)`
  (`decode.rs:9643`, `:9682`, `:9696`); the new
  `fill_lf_grid_rect_after_vartx` is the same fill with the band publish
  suppressed, and the four var-tx bodies call it. Nothing else about the fill
  changes — the LUMA/UV transform-size grids, the skip grid, the ref and
  delta-lf grids and the intrabc coverage stamp are untouched.
* The suppression is sound only if the tree reader publishes in EVERY arm, so
  the two LOSSLESS arms — which returned a leaf grid and published nothing,
  where libaom's `else` branch runs `set_txfm_ctxs(TX_4X4, xd->width,
  xd->height, 0)` — now publish `(4, 4)` over the block:
  `read_block_tx_size_rect` (`decode.rs:30020`) and `decode_rect4_16_intrabc`
  (`decode.rs:18185`). On a fully lossless frame this is unobservable (a
  lossless segment fails the var-tx condition, so nothing reads the bands);
  it is observable on a frame with a lossless SEGMENT, and it is what libaom
  writes.
* No decode arithmetic changed. `above_txfm`/`left_txfm` initialisation
  (`TXFM_CTX_INIT`), the context arithmetic and the reader recursion are all
  as they were.

## 4. Numbers

### 4.1 The two cells — before/after, live `aomdec --rawvideo`, per frame

| cell | before Y / U / V, exact | after Y / U / V, exact |
|---|---|---|
| `420_oddheight_320x236_diverging.obu` (16562 B, sha256 `84e4d1ab…`) | **234349 / 56957 / 52981, 0/16** | **0 / 0 / 0, 16/16** |
| `322x248` cell (16131 B, sha256 `9e8c9c6c…`) | **36286 / 13359 / 13001, 0/16** | **0 / 0 / 0, 16/16** |

The "before" column was measured on THIS tree by `git stash`-ing the
`decode.rs` hunk and rebuilding — it is not a citation:
`before_cell.obu → Y 234349 U 56957 V 52981 0/16` and
`before_cell_322x248.obu → Y 36286 U 13359 V 13001 0/16`, matching
`lanes/av1oddluma.report.md:19-20` and `lanes/av1cmpframe.report.md:120-121`
digit for digit. Frame 0 alone was wrong before, so nothing here is
propagation.

### 4.2 The `322x248` cell's provenance (it is NOT a committed fixture)

`322x248` exists only as live-encoded measurements in three reports; no
bytes are committed. I reproduced it with the recipe that
`lanes/av1422anom.report.md:200-208` records for the committed `320x232`
pin, after **validating that recipe against the committed pin first** (it
reproduces `420_oddheight_320x232.obu` byte-for-byte, 15773 B, sha256
`6832c3cd…`), and then:

```
ffmpeg -v error -f lavfi -i testsrc2=size=322x248:rate=24:duration=1 -pix_fmt yuv420p -y p248.y4m
aomenc --codec=av1 --profile=0 --input-bit-depth=8 --limit=16 --lag-in-frames=25 \
       --auto-alt-ref=1 --enable-global-motion=1 --pass=1 --cq-level=45 --threads=4 \
       --kf-min-dist=0 --kf-max-dist=999999 --width=322 --height=248 --cpu-used=0 p248.y4m -o Y248.webm
ffmpeg -v error -i Y248.webm -c copy -f obu -y cell_322x248.obu
```

→ 16131 B, sha256 `9e8c9c6c202a9d6a200e5393a79df5b6b5734ded3d004e3eac65f272c6e4730f`,
which is the sha256 `lanes/av1cmpframe.report.md:120` and
`lanes/av1gapremeasure.report.md:94` record for that cell. So the cell
measured here IS that cell.

### 4.3 The sibling grid — every committed fixture vs the live oracle

104 committed `.obu` fixtures, swept with a throwaway script
(`/tmp/oddwork/sweep.py`, not committed): each decoded by live `aomdec
--rawvideo` and by `decode_probe`, compared byte for byte per frame.

* **62 cells comparable: 62/62 byte-exact, every frame exact, on all three
  planes.** No cell moved to a non-exact reading; the two odd-height cells are
  among them and are the only ones that changed state.
* 41 cells the throwaway could not compare (the probe refuses them by name,
  or they are 10/12-bit and its byte packing differs in size from
  `aomdec`'s) and 1 the oracle refuses (`440_request_is_422.obu`). Those are
  covered by the crate's own gates in §5, not by my script — I am not
  claiming them.

### 4.4 The `+1` oracle-flip control

`the_counting_oracle_diff_detects_one_flipped_oracle_byte` and
`the_counting_oracle_diff_attributes_planes_per_frame` both pass on this
tree (2 passed, 0 failed). The flip control is the crate's own and does not
depend on any witness diverging, which is what makes a `0/0/0` reading worth
anything here.

## 5. Gate, and its mutation proof

**Committed gate (byte-exactness against the live oracle, not a route
counter, not a source scan):**
`stream::tests::the_pinned_420_oddheight_320x236_witness_decodes_byte_exact`
— `crates/ec-av1/src/stream.rs:11882-11939`. It pins the fixture
(16562 B, fnv1a64 `521889434652397870`, sha256 `84e4d1ab…` in the comment),
asserts `(0, 0, 0)` on all three planes and `exact == frames == 16`, through
the existing per-frame, per-plane counting comparator.

**Mutation proof (revert → reds BY NAME).** Reverting only the four call
sites (`fill_lf_grid_rect_after_vartx` → `fill_lf_grid_rect`) and rebuilding:

```
---- stream::tests::the_pinned_420_oddheight_320x236_witness_decodes_byte_exact stdout ----
panicked at crates/ec-av1/src/stream.rs:11924:9:
assertion `left == right` failed: the_pinned_420_oddheight_320x236_witness_decodes_byte_exact:
  the pinned 320x236 cell must decode byte-exactly against `aomdec --rawvideo` on every plane...
  left: (234349, 56957, 52981)
 right: (0, 0, 0)
test result: FAILED. 0 passed; 1 failed
```

With the fix restored the same test is `ok`. The gate bites the exact numbers
the defect produced, and it bites them in the plane the defect was in.

**Second test added (the class claim, not the cell):**
`decode::txfm_partition_update_rect_extent_is_the_node_not_the_leaf`
(`decode.rs:29281`) pins the extent rule from libaom's own
`mi_size_wide`/`mi_size_high` rows and checks the primitive on a RECT node —
the case the ticket suspected. Green.

**Regression** — `cargo test -p ec-av1 --lib -- 420 422 444 lossless warp
intra --skip bitrate_target_lands_within_5_percent_over_48_frames`: see §5.1.

## 5.1 Regression counts

```
cargo test -p ec-av1 --lib -- 420 422 444 lossless warp intra \
    --skip bitrate_target_lands_within_5_percent_over_48_frames
test result: ok. 181 passed; 0 failed; 2 ignored; 0 measured; 608 filtered out; finished in 279.71s
```

181 passed / 0 failed / 2 ignored, with
`stream::tests::the_pinned_420_oddheight_320x236_witness_decodes_byte_exact`
among them -- green in the same run the mutation turns red. The new
`decode::txfm_partition_update_rect_extent_is_the_node_not_the_leaf` is
outside that name filter, so it is reported separately: `1 passed; 0 failed`.

## 6. Class sweep — every writer and every reader of the `TXFM_CONTEXT` grid

`above_txfm` / `left_txfm` are written through exactly one primitive,
`txfm_partition_update_rect` (`decode.rs:29145`), plus four sites that repeat
its cell loops verbatim. Checked:

| site | what libaom does | verdict |
|---|---|---|
| `txfm_partition_update_rect` (`:29145`) | `txfm_partition_update`: extent from `mi_size_{wide,high}[txsize_to_bsize[txb_size]]`, value from `tx_size_{wide,high}[tx_size]` | **correct**, rect included — extent is the node, value is the leaf. Pinned by the new test. The identity that makes `txb/MI` the right cell count (`mi_size_* * MI == tx_size_*`) is now asserted, not assumed. |
| `read_var_tx_size` leaves (`:29433`) | `txfm_partition_update(above+blk_col, left+blk_row, tx_size, tx_size)` at the depth cap, the `!split` return and the recursion's `!split` | **correct**, including the `sub_txs == TX_4X4` early return, which is the one place a RECT node extent is used on a split path (`(4,4)` value over the parent's `(tx_w,tx_h)` extent) and matches libaom's `txfm_partition_update(..., TX_4X4, tx_size)`. |
| `read_block_tx_size_rect` (`:30050`) | `parse_decode_block`'s vartx branch | **was publishing the block's size over the tree**; its skip / TX_MODE_LARGEST / unsplit / 128-root arms each publish what libaom's own branch publishes, and its lossless arm now publishes `TX_4X4`. Fixed. |
| `decode_intrabc_rect` / `decode_intrabc_128rect` / `decode_intrabc_owned_rect` / `decode_rect4_16_intrabc` (`:14727`, `:15357`, `:17820`, `:18507`) | no whole-footprint publish exists on the var-tx path | **were the defect**; now call `fill_lf_grid_rect_after_vartx`. |
| `publish_txfm_bands_if_in_inter` (`:29116`) and the two verbatim loop pairs in `decode_intra_rect_in_inter` (`:13636`) and `decode_rect_split` (`:14076`) | `set_txfm_ctxs`: `tx_size_wide` over `n4_w`, `tx_size_high` over `n4_h` | **correct for rect** — they key the extent off the block's own `bw`/`bh` and the value off the block's resolved `(tx_w, tx_h)`, which is `set_txfm_ctxs` with the `skip && is_inter` term. Not "correct only because the node is square": the loops are extent-agnostic. |
| `decode_rect4_16_intrabc`'s own `if leaves.is_none()` publish (`:18499`) | — | already correct before this lane (it is the same rule, applied at the tree's own call site); kept. |
| `Neighbours::new` band init (`:9309` `TXFM_CTX_INIT`) | `av1_zero_above_context` / `av1_zero_left_context` memset both bands to `tx_size_high[TX_SIZES_LARGEST]` = 64 | unchanged, untouched by this lane. |
| readers: `txfm_partition_ctx_rect` (`:29058`) | `txfm_partition_context` | line-for-line identical, rect operands included; unchanged. |
| readers: `tx_size_context_txfm_rect` (`:28800`) and its square form (`:28912`) | `get_tx_size_context` | unchanged; the `inter_at` / `blk_side_at` override is the lane-av1txctxband fix and is out of this lane's scope. |

Sites that are **structurally** unable to hit this class, named so a later
lane does not re-audit them: every inter-frame caller
(`decode_inter_block`, `decode_inter_block8`, `decode_inter_sub8_*`) — on an
inter frame `fctx.intra_only` is false, so `fill_lf_grid_rect`'s band publish
does not run at all, and those bodies publish at their own sites.

**Two findings that were latent, both now closed with the fix:**
1. The four bodies above (a wrong publish that clobbers per-leaf sizes).
2. The two lossless arms (no publish where libaom writes `TX_4X4`) — found by
   asking what the suppression in (1) would leave behind, which is the only
   way to know the suppression is sound.

## 7. What the predecessor got wrong, for the record

* §4's "the `left` band one block over differs" was right about the SHAPE
  and wrong about the mechanism: the mi(44,58) block's split children
  published correctly, and the block's own tail publish overwrote them.
* The `txfm_partition_update_rect` rect-extent hypothesis (inherited from
  the ticket) is refuted in §6 with a test.
* The stale "fork at coefficient read 6 / 312 extra bits" lines remain in
  `lanes/av1labelspace.report.md:94-101` and
  `lanes/av1partadvance.report.md:88-98`; §4 of the predecessor's report is
  the replacement, and this report supersedes both on the fork's location.

## 8. `not_done` / handover

* **The `322x248` cell is not committed as a fixture.** Its measurement is
  here with a sha256-verified recipe (§4.2), but adding a 16131-byte binary
  fixture also means touching `scripts/fixture-library.tsv` and
  `scripts/verify-fixture-library.sh`, which is Main's call, not a lane's.
  Say the word and it is a two-line follow-up.
* **`EC_TXCTXB` is still not in the generator.** `get_tx_size_context`'s
  operand print (`pred_common.h:369-377`) is in the oracle tree by hand, the
  same rung-loss class this lane just fixed for `EC_VARTX`/`EC_VARTXCTX`. Not
  mine to add unasked; it is the obvious next rung.
* **The oracle tree carries my new rung.** It is derived from
  `scripts/instrument-aom-oracle.sh`, so a rebuild keeps it, and
  `scripts/check-aom-oracle-rungs.sh` does not yet assert it — a
  follow-up there would pin the derivation.
* **No sibling regressed**, and the 41 fixtures my throwaway sweep could not
  compare rest on the crate's own gates (§5.1), not on my script.
* Nothing is pushed and nothing is merged. The branch carries the
  predecessor's instrument commit plus this lane's fix, gate, test and
  report; Main owns the merge.
