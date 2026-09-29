# lane-av1pinspec — give each of the 43 unpinned count sites a spec-derived pin

Branch `lane-av1pinspec`, worktree `~/.cache/wt/av1pinspec`, base `3b691e13` (wave-3e
main, with `pins5` + `txsizeaudit-r5` already merged). **Ten commits, test/comment
changes only — no decoder logic, and `decode.rs` is not touched at all.**

```
8a8a9717 gate_coverage: lower the count-vacuity unpinned ceiling 43 -> 2
2375641e stream: pin the encode's frame count above 7 oracle calls (palette, rect-tx tool, screen intrabc, 16x4 pair)
38cf6255 stream: pin the encode's frame count in the intrabc rect/census family
37bc27ef stream: pin the ignored rect-tx recipe sweep and the 32-level AB gate
95e950fa stream: pin the restoration sweep and edge32_gate, whose spec is a parameter
71a190aa stream: pin the band and chroma-rect sweep family to one frame each
d960f5d3 stream: hoist the narrow-kernel gate's FRAMES assert above its oracle call
b5a45b2d stream: pin four more oracle counts, one of them off the wire itself
c887fa7f stream: pin eight single-key-frame oracle counts, three of them by hoisting
```

## 0. The fix shape, and what it changes about what a site counts

Every fix adds **the ENCODE's own frame count** as an `assert_eq!` **above** the
`ffmpeg_decode_sequence*` call, so the value later spent as ffmpeg's frame budget is
already proven against the encode instead of read off our own decode.

**It does not change what a site counts.** The call still spends `frames.len()`; the
assert above it makes that value a spec. This is deliberate, and it is the shape the
**17 already-pinned sites** already use — e.g. `stream.rs:5571` asserts
`decoded.len() == 4` and `:5576` then spends `decoded.len()` as ffmpeg's budget. The
sweep's population therefore stays at **60** and the change is visible as
`17 -> 58 spec-pinned` rather than as sites quietly disappearing.

The alternative — spending a named constant in place of `.len()`, the r7 shape at
`stream.rs:3895` — is strictly stronger per site, but it deletes the site from the
sweep, and 43 deletions would put the population at 17 against a **floor of 40**
(`gate_coverage.rs:2390`). The floor is not lowered here, so the assert-above shape is
the one that keeps the class enumerable. That trade is the one judgement call in this
lane; everything else is mechanical.

**Why the assert must sit above the call, in one line.** A wrong count reds either
way, but with the assert below, `ffmpeg_decode_sequence` has already been handed the
same short count, so its `out.stdout.len() == frame_bytes * frames` assert fires
first and the message blames ffmpeg for a number our decoder chose — the
misattributed-red class this whole sweep exists to name. Above the call, the failure
names the encode.

## 1. The sweep line, before and after

BEFORE, on `3b691e13`:

```
$ cargo test -p ec-av1 --lib -- count_vacuity_tests::every_locally_derived_oracle_count_is_reported --nocapture
count-vacuity sweep: 60 site(s); 17 spec-pinned by a prior assert, 43 not
```

AFTER:

```
$ cargo test -p ec-av1 --lib -- count_vacuity_tests --nocapture
count-vacuity sweep: 60 site(s); 58 spec-pinned by a prior assert, 2 not
ROW 5631 | an_svt_screen_palette_block_with_a_split_transform_decodes_exactly | decoded.len() | bucket=a | candidate=- | spec_in_scope=- | stream read from disk: count belongs to the pin | pixel_comparing=true
ROW 30924 | a_real_aomenc_stream_with_restoration_reads_lr_symbols_correctly | pics.len() | bucket=b | candidate=- | spec_in_scope=- | live aomenc encode: count belongs to the encoder | pixel_comparing=true
test gate_coverage::count_vacuity_tests::every_locally_derived_oracle_count_is_reported ... ok
test gate_coverage::count_vacuity_tests::the_count_vacuity_sweep_finds_the_known_sites ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 760 filtered out
```

**`43 -> 2`. No site disappeared** — 60 before, 60 after — so the ceiling moves
**down** `43 -> 2` because 41 sites stopped being unpinned. The ceiling is not
raised. The floor (`>= 40` sites) and the `frames.len()` shape anchor are untouched.

## 2. The 43-row table

`b:` = line on `3b691e13`, `a:` = line on `lane-av1pinspec` (the sweep's own numbers
after the change). `value` is what the new assert compares against.

| b: | a: | gate | expr | value | bucket | disposition |
|---|---|---|---|---|---|---|
| 5631 | 5631 | `an_svt_screen_palette_block_with_a_split_transform_decodes_exactly` | `decoded.len()` | — | **b** | hand forward, §3 |
| 10513 | 10522 | `a_real_aomenc_palette_stream_with_8x8_leaves_decodes_pixel_exact` | `frames.len()` | `1` | a | fixed |
| 10732 | 10758 | `rect_tx_tool_gate` | `decoded.len()` | `15` | a | fixed |
| 10734 | 10760 | `rect_tx_tool_gate` | `decoded.len()` | `15` | a | fixed (one assert covers both) |
| 11212 | 11248 | `a_real_aomenc_screen_key_frame_reads_use_intrabc_on_rect_strips` | `frames.len()` | `1` | a | fixed |
| 11383 | 11422 | `a_16x4_intrabc_pair_strip_decodes_pixel_exact` | `frames.len()` | `1` | a | fixed, assert hoisted |
| 11455 | 11500 | `a_lossless_16x4_chroma_pair_repairs_the_measured_site` | `frames.len()` | `1` | a | fixed |
| 11838 | 11888 | `a_coded_rect_intrabc_block_reconstructs_in_both_orientations` (arms) | `frames.len()` | `1` | a | fixed |
| 11981 | 12041 | same gate (skip_arms) | `frames.len()` | `1` | a | fixed |
| 12224 | 12292 | `an_sb128_screen_stream_with_intrabc_decodes_pixel_exact` | `frames.len()` | `1` | a | fixed |
| 12226 | 12294 | same gate, 8-bit arm | `frames.len()` | `1` | a | fixed (one assert covers both) |
| 12612 | 12689 | `a_sub8_leaf_census_over_intrabc_screen_streams_measures_the_sub8_refusal` | `frames.len()` | `1` | a | fixed |
| 12687 | 12773 | `an_intrabc_vartx_census_measures_the_mixed_leaf_refusal` | `frames.len()` | `1` | a | fixed |
| 12791 | 12883 | `a_real_aomenc_intrabc_mixed_vartx_tree_decodes_without_the_mixed_leaf_refusal` | `frames.len()` | `1` | a | fixed |
| 13093 | 13191 | `an_intrabc_block_under_tx_mode_select_decodes_pixel_exact` | `frames.len()` | `1` | a | fixed |
| 13154 | 13267 | `a_real_aomenc_stream_with_cdf_update_disabled_decodes_pixel_exact` | `frames.len()` | wire frame headers | a | fixed |
| 13360 | 13481 | `a_real_aomenc_rect_strip_palette_decodes_pixel_exact` | `frames.len()` | `1` | a | fixed |
| 13362 | 13483 | same gate, 8-bit arm | `frames.len()` | `1` | a | fixed (one assert covers both) |
| 15497 | 15629 | `ten_bit_tool_gate` (the sweep labels the gate `first_diff`) | `decoded.len()` | `frames` param | a | fixed |
| 27636 | 27773 | `a_real_aomenc_stream_with_a_coded_rect_strip_below_16x16_decodes_pixel_exact` | `frames.len()` | `1` | a | fixed, assert hoisted |
| 27764 | 27906 | `a_real_aomenc_stream_whose_square_block_reads_a_sub16_neighbours_mode_decodes_pixel_exact` | `frames.len()` | `1` | a | fixed, assert hoisted |
| 27933 | 28083 | `a_real_aomenc_stream_whose_chroma_edge_filter_reads_a_sub16_neighbours_uv_mode_decodes_pixel_exact` | `frames.len()` | `1` | a | fixed, assert hoisted |
| 27935 | 28085 | same gate, 8-bit arm | `frames.len()` | `1` | a | fixed (one assert covers both) |
| 28129 | 28280 | `a_real_aomenc_sb128_stream_whose_skipped_cfl_and_1to4_chroma_pairs_decode_pixel_exact` | `frames.len()` | `1` | a | fixed |
| 28131 | 28282 | same gate, 8-bit arm | `frames.len()` | `1` | a | fixed (one assert covers both) |
| 28456 | 28612 | `a_real_aomenc_stream_with_ab_partitions_below_16x16_decodes_pixel_exact` | `frames.len()` | `1` | a | fixed |
| 28458 | 28614 | same gate, 8-bit arm | `frames.len()` | `1` | a | fixed (one assert covers both) |
| 28609 | 28777 | `sweep_rectx_recipes` (`#[ignore]`) | `frames.len()` | `1` | a | fixed |
| 29212 | 29386 | `a_real_aomenc_stream_with_a_32_level_ab_partition_decodes_pixel_exact` | `frames.len()` | `1` | a | fixed |
| 30646 | 30826 | `a_real_aomenc_stream_with_restoration_reads_lr_symbols_correctly` | `pics.len()` | `1` | a | fixed |
| 30744 | 30924 | same gate — a `///` doc comment | `pics.len()` | — | **c** | not a count, §3 |
| 38225 | 38419 | `edge32_gate` | `decoded.len()` | `frame_count` (arm) | a | fixed |
| 38227 | 38421 | `edge32_gate`, 8-bit arm | `decoded.len()` | `frame_count` (arm) | a | fixed (one assert covers both) |
| 41953 | 42156 | `a_real_aomenc_stream_with_a_coded_strip_whose_chroma_is_a_4to1_or_sub8_rect_decodes_pixel_exact` | `frames.len()` | `1` | a | fixed |
| 41955 | 42158 | same gate, 8-bit arm | `frames.len()` | `1` | a | fixed (one assert covers both) |
| 42190 | 42398 | `the_chroma_rect_gates_excluded_seed_46_decodes_pixel_exact` | `frames.len()` | `1` | a | fixed |
| 42192 | 42400 | same gate, 8-bit arm | `frames.len()` | `1` | a | fixed (one assert covers both) |
| 42359 | 42572 | `a_real_aomenc_band_stream_seed46_decodes_pixel_exact` | `frames.len()` | `1` | a | fixed |
| 42361 | 42574 | same gate, 8-bit arm | `frames.len()` | `1` | a | fixed (one assert covers both) |
| 42568 | 42787 | `a_real_aomenc_stream_whose_frame_edge_partition_bit_is_horz_decodes_pixel_exact` | `frames.len()` | `1` | a | fixed |
| 42570 | 42789 | same gate, 8-bit arm | `frames.len()` | `1` | a | fixed (one assert covers both) |
| 42794 | 43025 | `a_real_aomenc_rect_inter_block_predicts_chroma_with_the_narrow_kernel_pixel_exact` | `frames.len()` | `FRAMES` (=6) | a | fixed, assert hoisted |
| 42796 | 43027 | same gate, 8-bit arm | `frames.len()` | `FRAMES` (=6) | a | fixed (one assert covers both) |

**41 fixed, 1 handed forward (b), 1 not a count at all (c).** 6+8+4+8+2+3+8+2 = 41
sites across the eight `stream.rs` commits.

## 3. The two that are not fixed, and why

**(b) `stream.rs:5631` — `an_svt_screen_palette_block_with_a_split_transform_decodes_exactly`.**
The stream is named by `EC_AV1_SVT1_STREAM` (`:5609`) and read from that path
(`:5617`). It is **not a committed pin** — the gate's own doc comment says the crop is
never committed to `fixtures/` — so no encode recipe, no `--limit`, no `-frames:v`
and no frame count exists anywhere in the tree for it. The "12-frame stream" in the
doc comment (`:5592`) is prose about one capture, and the env var accepts any file, so
asserting `12` would pin a number nothing can check. The one derivable alternative is
to count the stream's own OBU frame headers with `Av1Parser` (the shape used at
`:13267`; the parser is already imported at `:19`), which would make the count
wire-derived instead of decode-derived. Deliberately not done: the gate **SKIPs** on
any box without that env var (the battery reports it `ok` in 0 s here), so such an
assert would ship unexercised, and an OBU header count over-counts whenever the stream
carries `show_existing_frame`, which cannot be checked without the bytes. Handed
forward with the recipe.

**(c) `stream.rs:30744` (now `:30924`) — a `///` DOC COMMENT, not a count.**
The matched line is

```
///     let reference = ffmpeg_decode_sequence(&stream, 192, 128, pics.len());
```

a quoted fragment of the pre-r4 body of `pinned_lr_sgr_stream_call_unique_dump`, in
the doc comment that explains why that gate now takes its count from `FRAMES`. The
sweep scans raw source lines and cannot tell a doc comment from code. The gate it
describes is already correct: it calls `ffmpeg_decode_sequence(&stream, W, H, FRAMES)`
with `const FRAMES: usize = 1;` and asserts `pics.len() == FRAMES`. Left in place and
reported; making the scanner comment-aware is a change to the guard, not a fix to a
count.

## 4. Where each derived value comes from

Six derivations, every one read out of the gate's own encode call:

1. **`-frames:v 1` / `-vframes 1` on the y4m** — `a_16x4_intrabc_pair_strip`
   (`:11333`), `a_lossless_16x4_chroma_pair_repairs_the_measured_site` (`:11414`),
   `..._coded_rect_strip_below_16x16` (`:27566`),
   `..._square_block_reads_a_sub16_neighbours_mode` (`:27793`),
   `..._chroma_edge_filter_reads_a_sub16_neighbours_uv_mode` (`:27852`),
   `..._skipped_cfl_and_1to4_chroma_pairs` (`:28056`),
   `..._ab_partitions_below_16x16` (`:28367`),
   `..._a_32_level_ab_partition` (`:29119`),
   `..._frame_edge_partition_bit_is_horz` (`:42490`),
   `sweep_rectx_recipes` (`:28747`). Where the source carries no `:rate=`, `-vframes`
   is the binding bound.
2. **`--limit=1`** — the 8x8-leaf palette gate (`:10467`), the screen key-frame
   intrabc gate (`:11145`), both loops of the rect-intrabc gate (`:11797`, `:11945`),
   `an_sb128_screen_stream_with_intrabc` and everything below it through
   `screen_intrabc_stream_at_depth` (`:12101`), the rect-strip-palette gate
   (`:13313`), and the band/chroma-rect family whose helper renders
   `duration=0.04:rate=25` with `-t 0.04` and no limit (`:42082`, `:42094`, `:42263`,
   `:42277`).
3. **`--limit=1` over `-t 0.2` at `rate=25` with `-vf tile=2x2`** — 5 input frames,
   1 tiled output frame: the two intrabc censuses, the mixed vart-x tree gate and the
   tx-mode-select gate.
4. **`rate=25 x -t 0.60`, no `--limit`** — `rect_tx_tool_gate`, 15 frames (`:10690`).
   The one multi-frame site; the count is named `const ENCODED_FRAMES: usize = 15;`
   because 15 is an arithmetic fact about the fixture, not a flag someone typed.
5. **A value already in scope at the call** — `const FRAMES: usize = 6`
   (`a_real_aomenc_rect_inter_block_predicts_chroma_with_the_narrow_kernel`, `:42670`,
   driving both `-vframes {FRAMES}` and `--limit={FRAMES}`); `ten_bit_tool_gate`'s own
   `frames` parameter (`:15446`, the same value handed to
   `encode_10bit_gradients_seed` at `:15469` — 1 for the key-frame callers, 24 for the
   `--lag-in-frames=16` inter ones); `edge32_gate`'s per-arm `frame_count` tuple field,
   which is what the y4m is built from (`duration = frame_count / 25.0`, `:38117`) and
   which aomenc runs uncapped, so 1 for the intra arms and 5 for the inter/straddling
   ones.
6. **The wire itself** — `a_real_aomenc_stream_with_cdf_update_disabled` asserts
   `frames.len() == headers`, where `headers` is this stream's own OBU frame-header
   count, parsed with `Av1Parser` in the loop already sitting above the call
   (`:13133-13147`). Derived from the bitstream rather than a literal, because the arm
   overrides the helper's `--limit=1` with `--limit=3` (`:13127`) and aomenc keeps the
   last occurrence of a repeated flag — so a literal `3` would be a claim about flag
   precedence, and `headers` is the fact.

## 5. Red-before

### 5a. Per group — the guard's own message, one run per commit

Each group's edits un-applied, the real guard run, then the tree restored:

| commit | group | sites | guard says |
|---|---|---|---|
| `2375641e` | A palette / rect-tx tool / screen intrabc / 16x4 | 6 | `8 unpinned count site(s), ceiling is 2` |
| `38cf6255` | B intrabc rect + census family | 8 | `10 unpinned count site(s), ceiling is 2` |
| `b5a45b2d` | C cdf-update / rect-strip-palette / ten_bit_tool_gate | 4 | `6 unpinned count site(s), ceiling is 2` |
| `c887fa7f` | D single-key-frame gates | 8 | `10 unpinned count site(s), ceiling is 2` |
| `37bc27ef` | E ignored sweep + 32-level AB | 2 | `4 unpinned count site(s), ceiling is 2` |
| `95e950fa` | F restoration sweep + edge32_gate | 3 | `5 unpinned count site(s), ceiling is 2` |
| `71a190aa` | G band + chroma-rect family | 8 | `10 unpinned count site(s), ceiling is 2` |
| `d960f5d3` | H narrow-kernel | 2 | `4 unpinned count site(s), ceiling is 2` |

Every row is `2 + that group's sites` against a ceiling of `2`, and every row is
`test result: FAILED`. Since the ceiling equals the number of sites that survive the
whole lane, **removing any one of the 41 asserts puts unpinned at 3 and reds the same
assert** — the constant and the per-group revert together are the per-site proof.

Whole-branch revert, same result:

```
$ git checkout 3b691e13 -- crates/ec-av1/src/stream.rs   # ceiling stays 2
$ cargo test -p ec-av1 --lib -- count_vacuity_tests
thread '...' panicked at crates/ec-av1/src/gate_coverage.rs:2441:9:
43 unpinned count site(s), ceiling is 2 -- a new one was added (lower the ceiling only when a site is actually fixed)
test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 760 filtered out
```

### 5b. The new asserts themselves bite — value mutation

One mutation per **derivation shape**, all six applied at once, on the real gates:

```
$ cargo test -p ec-av1 --lib -- --test-threads=4 <the six gates>
---- a_real_aomenc_8bit_stream_with_a_rect_transform_decodes_pixel_exact stdout ----
assertion `left == right` failed: ...[cq20/tx1]: the encode codes 14 frames, the decode showed 15
  left: 15
 right: 14
---- a_real_aomenc_10bit_rect_and_ab_partitions_decode_pixel_exact stdout ----
assertion `left == right` failed: ... seed 42: the encode codes 24 frame(s), the decode showed 24
  left: 24
 right: 25
---- a_frame_edge_straddling_band_decodes_pixel_exact stdout ----
assertion `left == right` failed: ... 192x68 cq35 frames=5 10bit=false tile_cols=0: the encode codes 5 frame(s), the decode showed 5
  left: 5
 right: 6
---- a_16x4_intrabc_pair_strip_decodes_pixel_exact stdout ----
assertion `left == right` failed: a_16x4_intrabc_pair_strip_decodes_pixel_exact: expected one key frame
  left: 1
 right: 2
---- a_real_aomenc_stream_with_cdf_update_disabled_decodes_pixel_exact stdout ----
assertion `left == right` failed: ... the stream codes 3 frame(s), the decode showed 3
  left: 3
 right: 4
---- a_real_aomenc_rect_inter_block_predicts_chroma_with_the_narrow_kernel_pixel_exact stdout ----
assertion `left == right` failed: ... not every coded frame was decoded (192x128, depth=8, cq=20)
  left: 6
 right: 5

test result: FAILED. 0 passed; 6 failed; 0 ignored; 0 measured; 756 filtered out
```

The mutations were `ENCODED_FRAMES 15 -> 14`, `frames -> frames + 1`,
`frame_count -> frame_count + 1`, `1 -> 2`, `headers -> headers + 1`,
`FRAMES -> FRAMES - 1`. Every mutant fails, and **the "decode showed" column is the
proof the derived values are right, not just that the asserts exist**: this decoder
really produces 15 / 24 / 5 / 1 / 3 / 6 frames on those six fixtures, and the number
came out of the encode recipe, not out of the decode. Reverted, and all six green
again.

## 6. Green

Compile clean, no warnings:

```
$ cargo check -p ec-av1 --all-targets
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.14s
```

Every gate this lane touched, plus the whole `gate_coverage` module, plus all nine
`ten_bit_tool_gate` callers and all three `edge32_gate` callers — 57 tests,
`aomenc` present and required (`EC_AV1_REQUIRE_AOMENC=1`):

```
$ cargo test -p ec-av1 --lib -- --test-threads=4 <39 gates> gate_coverage
test result: ok. 57 passed; 0 failed; 0 ignored; 0 measured; 705 filtered out; finished in 128.10s
```

The `#[ignore]`d one is run explicitly:

```
$ cargo test -p ec-av1 --lib -- --ignored sweep_rectx_recipes
test stream::tests::sweep_rectx_recipes ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 761 filtered out; finished in 14.72s
```

And the acceptance filter:

```
$ cargo test -p ec-av1 --lib -- gate_coverage pin_inventory count_vacuity
test gate_coverage::pin_inventory_tests::the_pin_scanner_sees_every_shape ... ok
test gate_coverage::pin_inventory_tests::the_pin_scanner_sees_the_directory_literal_shape ... ok
test gate_coverage::count_vacuity_tests::every_locally_derived_oracle_count_is_reported ... ok
test gate_coverage::count_vacuity_tests::the_count_vacuity_sweep_finds_the_known_sites ... ok
test gate_coverage::pin_inventory_tests::every_pin_a_gate_reads_is_committed_under_the_crate ... ok
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 755 filtered out
```

## 7. Two things worth recording

**A first attempt at the commit split corrupted one hunk, and the guard caught
nothing.** `git apply --cached --unidiff-zero` re-anchors a hunk by line number
alone, so the narrow-kernel assert landed inside a *refusal* arm — inside the
`if msg.contains("unsupported")` block, before its `continue` — where it would only
have run on arms the gate had already decided not to compare. The lane's own green
run would not have noticed, because the compared arms never reach that line. The
commits were rebuilt with exact-text, context-anchored replacement and an
`assert count == 1`, and the assembled branch was then checked **byte-identical** to
the tree the battery and the mutation run had been executed against. A commit-splitting
tool that can drop a gate assertion silently needs the same red-before discipline as
the code it is splitting.

**The global `pre-commit` hook re-runs rustfmt on staged Rust** (`core.hooksPath` ->
`~/.omp/agent/hooks/git`), so two of the new asserts are committed wrapped across
lines even though they fit in 100 columns. Left as the hook produced them; the two
forms are the same assert with the same operands, and the wrapped form is what the
final verification run in §6 was executed against.

## 8. Not done / deferred

- `stream.rs:5631` (the SVT gate) — no derivable frame count; the exact recipe and
  the two reasons not to guess are in §3.
- The `///` doc-comment site the sweep cannot see — left as-is and reported, §3.
- The *value* mutation in §5b runs one gate per derivation shape (six), not all 41
  individually. The guard-level per-group revert in §5a is the per-site proof; §5b is
  what shows the asserts bite on the gates themselves.
- `an_svt_screen_palette_block_with_a_split_transform_decodes_exactly` and the
  census gates' `pixel_comparing` column are the guard's own approximation, not a
  re-measurement; the classification in §2 is from reading the gates, and each row
  records the value that reading produced.
- No full `cargo test -p ec-av1` run — project-wide validation is Main's.
