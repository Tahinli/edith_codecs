# lane-av1bookkeep — two bookkeeping items in `refusal_inventory.rs` and the 4:2:2 corpus gate

**Outcome in one line:** the refusal inventory listed 33 strings and `PROVEN` carried 31
rows — both missing rows are now registered, red-proven by deletion, and a new test holds
the two sets one to one; and the seventh measured-diverging 4:2:2 cell is pinned in
`the_pinned_422_corpus_cells_decode_pixel_exact`, re-measured byte-exact **on this tree**
(0/0/0), with the corpus re-measured 51/51 on the same tree.

Branch `lane/av1bookkeep`, base `b8385c93`, two commits, no push, no merge.
Worktree `/home/tahinli/.cache/wt/av1bookkeep`, `CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1bookkeep`.

---

## 1. W1-4 — the inventory listed 33 strings and PROVEN carried 31 rows

### 1.1 The gap, measured before any edit

A textual pass over the two tables at `b8385c93` (`REFUSALS` 32 entries +
`CAPABILITY_CLAIMS` 1 = **33 listed**; `PROVEN` = **31 rows**) gives exactly two listed
strings with no row:

| listed string | why it had no row |
|---|---|
| `a block size 4x8, 8x16 or 16x4 (or 8x4 at 4:4:0) has no chroma plane block at this frame's subsampling mode (libaom: …decodeframe.c:1456…)` | the BLOCK_INVALID subsize guard `refuse_invalid_plane_block` (`decode.rs`), added by lane-av1unwritten / lane-av1subsizesweep |
| `filter intra on a superblock-level HORZ/VERT strip (never expected -- av1_filter_intra_allowed_bsize caps at 32x32)` | the single `CAPABILITY_CLAIMS` entry, lane-hdrlossless |

No row pointed at a string that is not listed (checked both directions), and the module
printed both numbers seventeen lines apart:

```
refusal inventory: 32 refusals + 1 capability claims, 31 proven
```

`every_proven_refusal_names_a_test_that_exists` walks **row → live refusal**, so a string
added to `REFUSALS` with no proving gate is invisible to it. Nothing walked the other
direction. That is why two rows could sit missing while the suite stayed green.

### 1.2 The two rows

**(a) BLOCK_INVALID subsize refusal → `a_422_header_over_a_420_tile_refuses_the_subsize_libaom_calls_corrupt`, `NegativeGate`.**

The charter named this gate, and the anchor rule is the reason it needed an edit first:
`gate_body` extracts the gate's own braces and the row must be *quoted inside that body*
(whole string, or its leading clause up to the first `(`). The gate quoted neither — its
assertion matched only the libaom citation **embedded in** the refusal
(`"invalid with this subsampling mode"`). So the row, as the gate stood, would have failed
its own checker for the wrong reason.

The gate now carries a `REFUSAL` const with the whole string and asserts it on the error.
This is also a strictly stronger gate, not bookkeeping:

| mutation | old assertion | new assertion |
|---|---|---|
| `decode.rs` guard reworded `no chroma plane block` → `no CHROMA plane block` | **green** (the `decodeframe.c:1456` citation survives) | **red**: `…the refusal must be this decoder's OWN string, whole… got: unsupported: AV1 tile (a block size 4x8, 8x16 or 16x4 … has no CHROMA plane block …)` |

Error path verified: `ec_core::Error::Display` is `unsupported: {what} ({why})`, so the
whole `why` reaches `err.to_string()`; the assertion passes unmutated.

**(b) filter-intra capability claim → `a_sb_level_horz_vert_strip_admits_no_filter_intra_symbol`, `Enumeration`.**

No edit needed: the gate's body already contains
`src.contains("\"filter intra on a superblock-level HORZ/VERT strip")`, which is the
claim's **leading clause** — the second form the checker accepts. The gate walks every
explicit arm of `filter_intra_size_class_rect` (none admits a 64 axis), drives the
square-delegate callee `filter_intra_size_class` at 32/64 directly, and asserts
`(64,32)`/`(32,64)` have no arm.

### 1.3 The invariant that makes the next one impossible

New test `every_listed_refusal_and_capability_claim_has_a_proven_row`: every string in
`REFUSALS` + `CAPABILITY_CLAIMS` has a row, and `listed == PROVEN.len()` (one to one).

**Red proofs** (delete the row, run the test — file restored after each):

| deleted row | result |
|---|---|
| `…refuses_the_subsize_libaom_calls_corrupt` | `FAILED` — `…listed as refusals / capability claims but no PROVEN row names a gate that proves them` |
| `…admits_no_filter_intra_symbol` | `FAILED` — same assertion, same panic site (`refusal_inventory.rs:2736`) |

Both red on the test's **own** assertion (rc 101 with a compiled binary — an earlier
attempt at this proof spliced the wrong line range and produced a compile error instead;
that is not a red proof and was discarded).

### 1.4 After

```
refusal inventory: 32 refusals + 1 capability claims, 33 proven
anchor strength: 26 rows quote the WHOLE refusal string, 7 quote its LEADING CLAUSE, 0 match neither
```

Anchor strength was 25 WHOLE / 6 CLAUSE / 0 neither before this lane: row (a) is a whole
match (the new `REFUSAL` const), row (b) a clause match. All 22 tests in the module pass.

---

## 2. The seventh cell — `s422_384x240`

### 2.1 What the gate's doc said, and what was true

`lanes/av1422lift.report.md` §7 pinned six of the seven cells
`lanes/av1422ffmpegbase.report.md` §2.1 measured as diverging, and held the rest of the
branch with:

> THIS BRANCH MUST NOT LAND UNTIL `s422_384x240` CLOSES AND THE CORPUS RE-MEASURES 51/51.

Two things had to be settled before the row could be added, and neither was taken on
trust from the charter:

1. **Is the cell closed?** `lanes/av1422tailskip.report.md` §1 says it is — byte-exact on
   16 displayed and 17 decode-order frames — and says the cause was **not** the reserved
   group-tail chroma SKIP arm the risk study's R4 blamed.
2. **Does it decode exact on THIS tree?** Measured, not quoted — §2.3.

The charter's "94-cell census on the merged tree" is **not** reproduced here and is not
cited: no such artifact exists under `lanes/` or `~/.cache/census422*/`. What this lane
measured is in §2.3 and §2.4, from this tree.

### 2.2 The row, and why no second copy of the stream

The seventh cell's bytes are already in the repository: lane-av1422tailskip committed the
sweep artifact as `crates/ec-av1/fixtures/422_palette_intra_in_inter_384x240_17f.obu`.
Verified identical to the sweep file before pinning:

| | |
|---|---|
| `~/.cache/census422/sweep/s422_384x240.obu` | 19 344 B, sha256 `bf1c658e…a4b815b`, fnv1a64 `0x923ccabfcb933e90` |
| `~/.cache/census422b/sweep/s422_384x240.obu` | same sha256 |
| `fixtures/422_palette_intra_in_inter_384x240_17f.obu` | same sha256, same fnv1a64 |

So the row pins the existing name rather than committing a second 19 KB copy of one
stream under a second name; the doc says so and names both hashes. (A copy was staged
while checking, then removed — the tree carries no new fixture.)

The doc's "Why these six" paragraph was **corrected, not extended**: 384x240 is 8- *and*
16-aligned, so the seventh cell is not in the `round_ss` / non-8-aligned-geometry class the
other six are in. It is the late-starting frame-1 alt-ref chroma residual
(`lanes/av1422late.report.md`) and is now described as its own class.

### 2.3 Exactness re-measured on this tree

`EC_AV1_REQUIRE_FFMPEG=1 cargo test -p ec-av1 --lib the_pinned_422_corpus_cells_decode_pixel_exact`
— ffmpeg as the oracle, display order, all seven rows:

```
[422_palette_intra_in_inter_384x240_17f.obu]: sha256 bf1c658e…a4b815b
[422_palette_intra_in_inter_384x240_17f.obu]: byte-exact vs ffmpeg (yuv422p, 16 frames);
    census delta units=[1992, 1254, 1256] coded=[955, 742, 854]
test result: ok. 1 passed; 0 failed
```

* **0 wrong samples in Y/U/V on all 16 frames** — every plane and frame is compared, and
  `assert_422_stream_pixel_exact` panics on the first differing sample.
* **Non-vacuous**: the per-plane coefficient-unit census reads 1992/1254/1256 units and
  955/742/854 units that coded at least one coefficient, so the compare is not vacuous
  with respect to 4:2:2.
* Prior measurement of the same bytes on earlier trees: `0/1780/1871`, first bad display
  frame 6, U row 174 col 32 (`lanes/av1422ffmpegbase.report.md` §3,
  `lanes/av1422luma.report.md` §6) — the same comparator family, so the movement from
  1780/1871 to 0/0/0 is a real change and not a different instrument.
* Sibling gate on the same bytes, also run here:
  `the_pinned_422_palette_intra_in_inter_cell_window_is_byte_exact` → **ok**,
  "17 decode-order frame(s) byte-exact against aomdec (1 hidden), 16 chroma palette unit
  window(s)".

**Row non-vacuity.** Mutating that row's fnv1a64 by one turns the gate red on its own
assertion: `…[422_palette_intra_in_inter_384x240_17f.obu]: bytes drifted`. Mutating the
**sha256** column stays green *by design* — it is the corpus's identity column for a
re-capture, not an in-process assertion (the code says so at the column); recording it so
the green is not mistaken for a weak gate.

### 2.4 The corpus re-measures 51/51 — also on this tree

The second half of the landing condition, run here rather than assumed:

* driver `~/.cache/census422b/ffcensus.py` (display order, shown frames, geometry from
  `ffprobe`, a count over zero frames is a hard error, `probe` recorded per row),
* oracle binary = `dump_yuv` built from **this lane's tree**
  (`CARGO_TARGET_DIR=/home/tahinli/.cache/tgt/av1bookkeep`, recorded in every result row),
* cell list = the census's own `cells_ff.json`, 4:2:2 subset selected by probing each
  stream's own sequence header.

| run | cells | result |
|---|---|---|
| 4:2:2 subset | 51 | **51 BYTE-EXACT**, incl. `s422_384x240` 0/0/0 |
| whole corpus | 95 | **95 BYTE-EXACT** (51 4:2:2 + 44 4:2:0 / 4:4:4 controls) |

All 95 stream paths were resolved and every remapped path's sha256 checked against the
census's recorded `sha` before use; 0 unresolved. Two honest limits on this instrument:
it is the **ffmpeg** oracle in **display** order (the aomdec decode-order census is a
different instrument and was not re-run), and the census's `probe_split_control`
("the clean build must refuse 4:2:2") is obsolete now that the lift has landed — it is not
part of this measurement.

---

## 3. Verification actually run on this tree

| check | result |
|---|---|
| `cargo check --tests -p ec-av1` | clean (one pre-existing `dead_code` warning, `dumpio.rs::pin_reporting`, untouched by this lane) |
| `cargo test -p ec-av1 --lib refusal_inventory` (22 tests) | 22 passed, 0 failed |
| `every_proven_refusal_names_a_test_that_exists`, `--nocapture` | ok; anchor strength 26/7/0, inventory 33 rows |
| `a_422_header_over_a_420_tile_refuses_the_subsize_libaom_calls_corrupt` | ok (also the guard-wording mutation: red) |
| `the_pinned_422_corpus_cells_decode_pixel_exact`, `EC_AV1_REQUIRE_FFMPEG=1` | ok, 7/7 rows (also the fnv mutation: red) |
| `the_pinned_422_palette_intra_in_inter_cell_window_is_byte_exact` | ok (sibling gate, same bytes) |
| 51-cell / 95-cell ffmpeg census, this tree's `dump_yuv` | 51/51 and 95/95 BYTE-EXACT |

The two full `ec-av1` suite runs this wave live on the VPS fleet; the scoped runs above are
what this lane executed locally.

## 4. Commits

1. `44afb448` — ec-av1: the inventory listed 33 strings and PROVEN carried 31 rows
   (`refusal_inventory.rs` +2 rows +1 test, `stream.rs` gate quote)
2. `1dd17d55` — ec-av1: the seventh 4:2:2 corpus cell is pinned, and re-measured exact here
   (`stream.rs` row + doc)

## 5. Not established here

* The charter's "94-cell census on the merged tree" figure — no artifact for it exists in
  `lanes/` or `~/.cache/census422*/`, so it is neither reproduced nor cited. What is
  measured is §2.4 (95/95 on this tree, ffmpeg/display-order instrument).
* The aomdec decode-order 51-cell census was not re-run.
* No new fixture was committed, by design (§2.2); if a later lane wants the cell pinned
  under its census name, that is a rename, not a new pin.