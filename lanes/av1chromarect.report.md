# lane-av1chromarect — 4:4:4 chroma extent + multi-unit walk at the 128-root intra-BC strip

Worktree `~/.cache/wt/av1chromarect`, branch `lane-av1chromarect`, base
`edffad9e` ("Merge lane-chromahalvings-r5: EC_HALVSWEEP rungs"). All edits are
absolute-pathed inside this worktree; `git -C ~/Documents/Code/Rust/edith_codecs
status --porcelain` printed nothing after every edit batch.

Target: `decode_intrabc_owned_rect` (`crates/ec-av1/src/decode.rs:16897`) — the
128-root intra-BC HORZ/VERT strip path (`BLOCK_128X64` / `BLOCK_64X128` and the
`decode_block_rect64` strips under them).

## 0. Provenance of the witness

`crates/ec-av1/fixtures/r512.obu`, 6948 B, git blob
`bff992e98d01fed6ec73ed35ddd73f9fa482d19f`. It is on **`lane-chromahalvings-r3`**
and on no other ref of that lane (`lane-chromahalvings` and
`lane-chromahalvings-r5` both lack it, and so does `main`):

    $ for r in lane-chromahalvings-r3 lane-chromahalvings lane-chromahalvings-r5 main; do
        printf "%s " "$r"; git cat-file -e "$r:crates/ec-av1/fixtures/r512.obu" 2>/dev/null \
          && git rev-parse "$r:crates/ec-av1/fixtures/r512.obu" || echo MISSING; done
    lane-chromahalvings-r3 bff992e98d01fed6ec73ed35ddd73f9fa482d19f
    lane-chromahalvings MISSING
    lane-chromahalvings-r5 MISSING
    main MISSING
    $ git hash-object crates/ec-av1/fixtures/r512.obu
    bff992e98d01fed6ec73ed35ddd73f9fa482d19f

Recipe (from `lanes/av1chromahalvings.report.md` r4, re-stated so the pin is
reproducible): testsrc2 512x512 yuv444p 8-bit, `--sb-size=128 --enable-intrabc=1
--enable-palette=1 --cq-level=50 --enable-rect-partitions=1
--enable-1to4-partitions=1 --min-partition-size=4 --max-partition-size=128
--tune-content=screen --profile=1 --obu`, 3 frames.

## 1. The defect, and the red-before MEASURED HERE

`decode_intrabc_owned_rect` sized its chroma plane block `(bw / 2, bh / 2)` and
passed `px / 2, py / 2` at eight sites as the chroma MV origins. libaom does not
halve there: `av1_get_max_uv_txsize` (blockd.h:1372) is
`av1_get_adjusted_tx_size(max_txsize_rect_lookup[get_plane_block_size(bsize, ss_x,
ss_y)])` and `get_plane_block_size` is `ss_size_lookup[bsize]`, so at ss (0,0)
the chroma plane block **is** the luma footprint. At 4:2:0 (`ss_x == ss_y == 1`)
`>> 1` and `/ 2` are the same arithmetic, which is why the bug was invisible
until a 4:4:4 witness existed.

Red-before, on the **unpatched** line in this worktree, per-plane sample counts
against the instrumented aomdec (`aomdec --codec=av1` with
`EC_AV1_FINAL_DUMP`, ours with `set_final_dump_prefix`; 512x512 8-bit 4:4:4,
786432 B per frame, three frames, both sides in decode order):

    f0  Y 166229 wrong from 65728 | U 155383 from 32848  | V 153498 from 32848   total 475110
    f1  Y       0 (byte-exact)   | U 74708 from 49282   | V 73501 from 49280    total 148209
    f2  Y       0 (byte-exact)   | U  9216 from 114880  | V  8192 from 229760   total  17408

Luma byte-exact on f1 and f2 while chroma is wrong is the signature that makes
this a chroma-plane sizing defect and not a block-geometry one. The numbers
reproduce the previous lane's r4 measurement exactly.

## 2. What the blocker's own refusal was actually asking for

The previous lane's r4/r5 finding was that the ss-derived extent REFUSES
`fixtures/r512.obu` with

    unsupported: AV1 tile (a rectangular inter chroma transform unit whose shape
    has no coefficient table set here)

and concluded the fix needs a new `TxbSet` — `ChromaRect32x64` / `ChromaRect64x32`
— with its own CDF set, `rect_scan` and rect `base_ctx_rect` / `br_ctx_rect`.
**That conclusion is wrong, and the wrongness is the finding.**

The refusal names the shape the crate HANDED `rect_inter_chroma_set`, not the
shape libaom codes. With the ss extent a 4:4:4 32x64 luma block's chroma plane
block is 32x64, and this crate read it as one rect transform. libaom never forms
a 64-point CHROMA transform:

* `get_vartx_max_txsize` (blockd.h:1447) —
  `return av1_get_adjusted_tx_size(max_txsize_rect_lookup[bsize]);` for `plane != 0`.
* `av1_get_adjusted_tx_size` (blockd.h, just above `av1_get_max_uv_txsize`) maps
  `TX_64X32 → TX_32X32` and `TX_32X64 → TX_32X32`, and returns everything else
  unchanged.
* `max_txsize_rect_lookup[BLOCK_SIZES_ALL]` (common_data.h:126) has
  `BLOCK_32X64 → TX_32X64`, `BLOCK_64X32 → TX_64X32`.
* `decode_token_recon_block` (decodeframe.c:994-1004) steps the plane block by
  `tx_size_*_unit[max_tx_size]`, i.e. 4 mi per axis, and
  `decode_reconstruct_tx`'s `if (tx_size == plane_tx_size || plane)` arm never
  recurses for chroma — the same statement from the other side.

So a 4:4:4 32x64 chroma plane block is **two stacked `TX_32X32` units**, each
with its own eob, its own scan and its own coefficient context. `TX_32X32` is
already in the crate: `rect_inter_chroma_set(32, 32) → TxbSet::Chroma32`
(decode.rs:28919, the arm `lane-av1-444` added). What was missing was not a
coefficient set but the **walk** that hands the crate `2 x 32x32` instead of
`1 x 32x64`.

The 64-axis truncation `read_inter_plane_rect` does internally
(`let (cw, ch) = (w.min(32), h.min(32));` + `extend_corner`, decode.rs:29532) is
libaom's `av1_get_max_eob` rule, and that rule is a LUMA rule: a chroma plane
block is never 64 points long in the first place. Routing a 4:4:4 32x64 chroma
plane block through one 32x64 unit would have produced a new `TxbSet` that no
stream can ever exercise — a coefficient table with no bitstream, which is the
worst kind of green.

### What shipped instead

* `read_intrabc_rect_chroma_split` (decode.rs, after
  `decode_intrabc_owned_rect`): the same `(cw.min(32), ch.min(32))` / `nx * ny`
  tiling `read_inter_rect_chroma` already implements for the other rect paths,
  for a caller that keeps its own grid assembly and neighbour recording. It is
  entered ONLY when `cw.min(32) != cw || ch.min(32) != ch`, so at 4:2:0 and at
  every 4:4:4 shape below 64 the caller's own one-unit read is untouched
  byte-for-byte.
* Per unit: its own `around_mi_rect(cu_mi, span_x, span_y)` gather, its own
  `(cpx + cu_col*uw, cpy + cu_row*uh)` origin, its own prediction slice, its own
  `covering_leaf_tx_type(...)` inheritance (`av1_get_tx_type`, blockd.h:1287,
  reads `xd->tx_type_map` at the unit's own position), the `get_txb_ctx` plus-10
  offset as `Some(3)`, and `record_mi_chroma` immediately so the next unit
  reads this one's context.
* `record_split_luma_rect_mi_modes` / `record_rect_mi_luma_only` (new, extracted
  from `record_split_luma_rect_mi` / `record_rect_mi`): when the chroma plane
  block was walked as several units, the whole-block coefficient stamp would
  overwrite the per-unit spans with one state (the `override-slot-on-one-arm`
  class, the same reason the lossless arm sets `mu_chroma = true`). The flag is
  `!skip && …`: a SKIPPED strip reads no chroma unit at all (its whole plane
  block is one `push_mc_rect` of `ZERO_RESIDUAL`), so no per-unit stamp ran and
  the whole-block stamp is still the only one that happens.

### The two pieces the charter asked for, and why they are not needed

* **CDF set** — `Chroma32` (`cdf_state.rs`), the exact
  `get_txsize_entropy_ctx(TX_32X32)` tables. Nothing new.
* **`rect_scan`** — a `TX_32X32` unit is square, so
  `read_inter_plane_rect` takes its `(cw, ch) == (32, 32)` branch and reads
  `default_scan(TX32)` (decode.rs:29546); `rect_scan` is never called. Nothing
  new.
* **rect `base_ctx_rect` / `br_ctx_rect`** — same reason: those are the rect
  forms of the 2:1 and 4:1 nz-map offsets, and no 2:1 or 4:1 chroma unit exists
  at 4:4:4 for this block shape. Nothing new. The call the new walk makes is
  byte-identical to the one `read_inter_rect_chroma` already makes for the
  4:4:4 four-unit arms proven by `lane-av1chrtx`.

## 3. Result on the witness, and the OPEN fork that remains

Same oracle, same comparison, after the fix:

    f0  Y       0 (byte-exact)   | U  7488 from 229728 | V  8128 from 229728   total 15616
    f1  Y       0 (byte-exact)   | U 22571 from 65704  | V 41782 from 65704    total 64353
    f2  Y       0 (byte-exact)   | U      0            | V      0                      0

Wrong samples over the whole stream: **640727 → 79969** (−87.5 %). Luma is
byte-exact on all three frames (786432/786432 samples; it was 166229 wrong on
f0). Frame 2 is byte-exact on all three planes (it was 9216 U + 8192 V wrong).

Reach proof for the site, `EC_HALVSWEEP=1` (the rung is main's, merge
`edffad9e`), five strips on this witness, `mi` in 4-px units:

    EC_HALV ibc_owned mi=(32,40)  bw=32 bh=64 skip=false
    EC_HALV ibc_owned mi=(48,64)  bw=32 bh=64 skip=true
    EC_HALV ibc_owned mi=(64,108) bw=16 bh=64 skip=true
    EC_HALV ibc_owned mi=(112,96) bw=32 bh=64 skip=true
    EC_HALV ibc_owned mi=(112,104) bw=32 bh=64 skip=true

(The previous lane reported 6 and named `mi=(72,32) 64x32 skip=false`. On this
base the count is 5 and that block is not reached. The witness blob is
byte-identical, so the difference is the tree, not the stream — the same
stale-verdict hazard the previous lane's r4 recorded, re-measured rather than
inherited.)

### The residual is a DIFFERENT site, and it is NOT this lane's

Both remaining regions are chroma-only, bounded, and luma-clean, so they are
reconstruction defects rather than entropy desyncs:

* f0: 128 dirty 8x8 cells, bbox x=352..=504, y=448..=504 — the last 64 rows of
  the frame. Strip #4's own footprint (x=384..=416) is CLEAN; strip #5's
  (x=416..=448) is dirty, and so is everything to its right.
* f1: 362 dirty 8x8 cells (U) / 667 (V), bbox x=128..=368 / 128..=408,
  y=128..=504, first dirty cell (168,128) PARTIAL (56 of 64 samples).

Ruled out by measurement, not by argument:

* **CfL.** `cfl_ac_q3_at` (decode.rs:20179) is the other `bw / 2, bh / 2` site
  in this wave and is out of scope (another agent owns it). A temporary probe at
  both its callers (`cfl_ac_q3`, `cfl_ac_q3_rect`) over the whole witness:

      $ EC_ZZCFL=1 cargo test -p ec-av1 --lib <witness> -- --nocapture | grep -c ZZCFL
      0

  The witness codes NO CfL block, so `cfl_ac_q3_at` cannot be the residual. (The
  probe was removed before the commit; it is not in the diff.)
* **The skip arm's record.** Forcing `chroma_split_units` true for skipped
  strips as well changes neither number — the f0 band and the f1 region are
  identical with and without it.
* **This lane's helper.** The per-unit prediction slice's column term
  (`cu_col * uw`) is dead on this witness (`nx == 1`), so mutating it leaves the
  gate green; the row term does bite (see §5, M5).

NOT DONE, explicitly: the f0 and f1 chroma residue above. It is not attributable
to `decode_intrabc_owned_rect` from the evidence I have, and I did not identify
its site. It is stated as a measured open fork, with numbers, rather than
absorbed into a "wrong by exactly N" expectation.

## 4. Gates

### 4.1 The refusal gate went in FIRST, and it went RED

Commit 1 (`58629693`) pins the witness, lands the ss-derived extent and the eight
MV origin pairs, and adds
`a_444_intrabc_owned_rect_strip_refuses_the_missing_rect_chroma_coefficient_set`
— which asserts, on purpose, that the witness is REFUSED with the exact string
`a rectangular inter chroma transform unit whose shape has no coefficient table
set here`, and that exactly one strip reached the corrected extent before the
decode stopped. It cites this lane in the test. Without it the witness would be
a pin with nothing watching it.

Commit 2 replaces that test with
`a_444_intrabc_owned_rect_strip_sizes_its_chroma_plane_block_and_decodes`, the
pixel-exactness assertion the refusal gate was there to force.

The refusal string is gone from the witness's decode because nothing reaches it,
not because it was deleted: `rect_inter_chroma_set` still refuses `(32, 64)` and
`(64, 32)`, the string is still in `decode.rs:28938` and in
`refusal_inventory.rs:55`, and the gate
`every_rect_transform_shape_the_census_lists_has_a_coefficient_table_and_scan`
(which names all three rect refusals and asserts they are still in the source)
still passes.

### 4.2 Non-vacuity: the route counter, and three mutations

`IBC_OWNED_RECT_CHROMA_FOOTPRINT_444_HITS` counts 128-root intra-BC strips whose
chroma plane block was sized at ss (0,0). At 4:2:0 the corrected and halving
forms are identical, so `>= 1` cannot be satisfied by a 4:2:0 stream, and the
arm is exclusive to this function. The gate asserts `hits == 5`.

Mutation proofs against the gate, each reverted after:

| # | mutation | result |
| --- | --- | --- |
| M1 | `bw >> ss_x` / `bh >> ss_y` and `px >> ss_x` / `py >> ss_y` back to `/ 2` | **RED** — `assertion left == right failed: 6 128-root intra-BC strip(s) sized at 4:4:4, not 5` (the counter bites: with the halving a sixth strip reaches the arm) |
| M3 | `(nx, ny) = (cw / uw, ch / uh)` → `(ch / uh, cw / uw)` | **RED** — `frame 0 plane Y diverges from the oracle at sample 65728 of 262144 (x=192, y=128)` |
| M5 | `let (cu_x, cu_y) = (cpx + cu_col * uw, cpy + cu_row * uh)` → `(cpx, cpy)` | **RED** — `frame 0 plane U diverges from the oracle at sample 65696 of 262144 (x=160, y=128)` |

A fourth mutation (dropping the `cu_col * uw` term from the prediction slice)
left the gate GREEN — correctly, because `nx == 1` on this witness makes that
term dead here. Recorded because a green mutation is a fact about coverage, not
about the gate.

The gate's own shape, per (frame, plane) exact prefix against the oracle, with
the floors it pins: `[[262144, 229728, 229728], [262144, 65704, 65704],
[262144, 262144, 262144]]` — the same honest "exact prefix that only grows" form
as the sibling `a_444_intrabc_rect_chroma_plane_block_is_the_block_footprint`.
It is a floor, not an exactness claim: §3 says exactly what it does not cover.

## 5. Gate families re-quoted

`cargo check -p ec-av1 --all-targets` clean (no errors, no warnings) before every
run, on a private `CARGO_TARGET_DIR`; a green gate alongside a build error would
have been a stale binary.

    $ EC_AV1_REQUIRE_AOMDEC=1 EC_AV1_REQUIRE_AOMENC=1 \
      cargo test -p ec-av1 --lib -- a_444 lossless_444 intrabc_rect 444_lossless -- --nocapture
    test result: ok. 33 passed; 0 failed; 0 ignored; 0 measured; 730 filtered out

That set contains the 4:4:4-lossless family (`a_lossless_444_*`,
`a_444_lossless_sb64_intrabc_rect_chroma_walks_4x4_units`,
`a_real_aomenc_lossless_444_key_frame_decodes_sample_exact`), the intrabc_rect
family (`a_444_intrabc_rect4_reads_its_own_chroma_plane_block`,
`a_lossless_444_intrabc_rect_leaf_walks_per_4x4_units`,
`a_lossless_444_intrabc_rect_replay_steps_by_the_units_own_mi_footprint`,
`a_skipped_lossless_intrabc_rect_strip_zeroes_its_entropy_bands`,
`a_coded_rect_intrabc_block_reconstructs_in_both_orientations`) and — the 4:2:0
identity arm — `a_444_intrabc_rect_chroma_plane_block_is_the_block_footprint`,
which decodes the pinned 4:2:0 twin (`420_intrabc_rect4_witness.obu`) through
`decode_all_frames_vs_oracle`.

The wider 121-test `rect intrabc ibc chroma 444 420 subsampl` family, tip vs
the base commit, is in §6.

## 6. 121-test family: tip vs base

One filtered run per tree, same filter string, same env, separate private
`CARGO_TARGET_DIR` each (a shared-shape target dir is exactly how the previous
lane got a stale verdict):

    $ EC_AV1_REQUIRE_AOMDEC=1 EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib \
        -- rect intrabc ibc chroma 444 420 subsampl -- --nocapture

| tree | result |
| --- | --- |
| base `edffad9e` (`~/.cache/wt/av1chromarect-base`, detached) | `ok. 137 passed; 0 failed; 5 ignored; 0 measured; 620 filtered out; finished in 288.38s` |
| tip `lane-av1chromarect` | `ok. 138 passed; 0 failed; 5 ignored; 0 measured; 620 filtered out; finished in 295.84s` |

The set difference of the two passing-test lists is exactly one line:

    34a35
    > test stream::tests::a_444_intrabc_owned_rect_strip_sizes_its_chroma_plane_block_and_decodes

The same 5 tests are `#[ignore]`d in both trees (two superblock-size globals, a
directional sweep, an intrabc probe needing the real-library manifest, and the
rectx recipe sweep). Nothing regressed, nothing was silently dropped.

Inside that family, the 4:2:0 identity is carried by
`a_444_intrabc_rect_chroma_plane_block_is_the_block_footprint`, which decodes
the pinned 4:2:0 twin `420_intrabc_rect4_witness.obu` through
`decode_all_frames_vs_oracle` (every frame byte-exact vs aomdec) and
`a_444_intrabc_rect4_reads_its_own_chroma_plane_block`; the 4:4:4-lossless arm
by the `a_lossless_444_*` set; the intrabc_rect arm by
`a_lossless_444_intrabc_rect_leaf_walks_per_4x4_units`,
`a_lossless_444_intrabc_rect_replay_steps_by_the_units_own_mi_footprint`,
`a_skipped_lossless_intrabc_rect_strip_zeroes_its_entropy_bands`,
`a_444_lossless_sb64_intrabc_rect_chroma_walks_4x4_units` and
`a_coded_rect_intrabc_block_reconstructs_in_both_orientations`.

## 7. Gap row — `decode_intra_sub8_leaf` has no `chroma_422` arm

`crates/ec-av1/src/decode.rs:45594`, body ends at `46422`. It carries a
`chroma_444` branch set and **zero** `chroma_422` mentions inside the body
(`awk 'NR>=45606 && NR<=46400' | grep -c chroma_422` → `0`; the two
`chroma_422` mentions at 45394/45406 belong to the enclosing
`decode_intra_split`, not to this function).

The 4:4:4 branches it owns, and what each falls back to at 4:2:2:

| line | what the `chroma_444` arm does | what 4:2:2 gets instead |
| --- | --- | --- |
| 46083 | `let chroma_444 = ss_x == 0 && ss_y == 0` | `false` at (1,0) |
| 46086 | `unit_x, unit_y = (px, py)` | `(cpx, cpy)` — the 4:2:0 double halving; at (1,0) the width halves and the height does not |
| 46088-46093 | `reach_c` = the piece's own rect reach at 4:4:4 | `group_reach` (an 8x8 group reach) |
| 46095 | `band_r, band_c` from `lmi` | from `r, c` (the group) |
| 46101-46102 | `smooth_uv_neighbour` sampled at `lmi` | sampled at `gr, gc` |
| 46115 | `record_uv_mode_mi(lmi, w_mi, h_mi)` | `record_uv_mode_mi(gr, gc, 2, 2)` — a 2x2 group stamp |
| 46122 | `cfl_src_rect(px, py, bw, bh)` | not taken |
| 46134, 46198 | the non-square 4:4:4 unit shape arms | not taken |
| 46340-46341 | chroma per-unit gather at `lmi` with a 4-px step | at `group_mi` with an 8-px step |
| 46399 | the 4:4:4 publish/stamp half | the `!chroma_444 && !chroma_422` fallbacks (45394/45406 in the enclosing `decode_intra_split`) |

The shape the missing arm would have to be: at 4:2:2 `ss_size_lookup` maps an
8x4 block to `BLOCK_INVALID` (common_data.c:38) and no `TX_4X8` chroma unit
exists, so a 4:2:2 sub-8 leaf's chroma is coded CHUNKED, exactly the
`invalid_ss_plane` case `read_inter_rect_chroma` already special-cases
(decode.rs:29328, `(4, 32)` at ss (1,0) → `(4, 8)` units).

**Caller set — exactly two sites, and BOTH are already 4:2:2-aware:**

1. `decode.rs:44724` — the `BLOCK_4X4` leaf of the sub-8x8 intra-in-inter split
   arm. It passes
   `has_chroma = chroma_444 || (cmi & 1 == 1 && chroma_422) || i == 3`
   (libaom's `is_chroma_reference`, `av1_common_int.h:1459`: BOTH odd-column
   pieces at 4:2:2).
2. `decode.rs:46610` — the `(bw, bh)` 8x8/4x8/8x4 piece. It passes
   `has_chroma = piece_is_chroma_ref(i)`, and `piece_is_chroma_ref`
   (decode.rs:46534) has an explicit `else if chroma_422 { !vert || ((gc + i) & 1) == 1 }`
   branch.

So the callers compute the 4:2:2 chroma-reference rule correctly and the leaf
body has no geometry arm to honour it. This is a latent gap, not a live bug: the
crate refuses 4:2:2 by name at the sequence header
(`"a chroma format of 4:2:2 (subsampling_x != subsampling_y): this decoder
decodes 4:2:0 and 4:4:4; 4:2:2 is not ported"`, `refusal_inventory.rs:119`, gate
`a_non_420_subsampled_sequence_header_is_refused_by_name`, stream.rs:2258), so
`chroma_422` is always `false` when this function runs. Not implemented here, by
charter.

## 8. Scope boundaries, stated

* `cfl_ac_q3_at` (decode.rs:20179, `let (cw, ch) = (bw / 2, bh / 2);`, no
  `fctx`) — **untouched, another lane owns it.** §3 measures that the witness
  codes no CfL, so it is not this witness's residual either.
* `suppress_internal_lf_edges` — **not re-fixed.** C1 (wave 3d, on this base)
  already threads `fctx` into it and computes `((w_mi * MI) >> ss_x(fctx)).max(4)`.
  The earlier "format-blind" claim is retracted upstream; there is nothing here
  to change.
* `decode_intrabc_rect` (the non-`owned` sibling, decode.rs:13646) reads its
  chroma with the same single-unit `rect_inter_chroma_set(cw, ch)`. It is
  ss-correct (lane-ibc444c) and its pinned 4:4:4 witness is 8x16, one unit, so
  the multi-unit case is latent there too. I did not widen the fix: this lane's
  witness and its gate do not reach it, and a change there would be unmeasured.
  Flagged for whoever takes the next 4:4:4 rect cell.

## 9. What I could not do

1. The f0/f1 chroma residue of §3 is not identified to a site. 79969 wrong
   samples remain out of 2359296 (3.4 %); luma is exact everywhere.
2. The 128-root multi-unit *record* difference in §2 is derived from libaom's
   per-unit above/left context update and validated by the gate going from
   640727 wrong samples to 79969 with luma exact; I did not diff our per-unit
   above/left contexts against the oracle's rung-by-rung.
3. No heavy encodes were run: the witness was taken from the pin, and the gate
   families are fixture-driven. No VPS fleet run was requested or consumed.
