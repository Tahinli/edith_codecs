# Merge wave 2 — 13 lanes into `main`

Base `main` = `298f75c4` (= `origin/main` at open). Every lane is merged
`--no-ff` as its own commit, in the order the review pass fixed. Nothing was
pushed. `CARGO_TARGET_DIR=$HOME/.cache/cargo-target`, `TMPDIR=$HOME/.cache/tmp`.

## Result

| # | branch | tip merged | conflicts resolved how | files touched |
|---|--------|-----------|------------------------|---------------|
| 1 | `lane-av1chrtx` | `ea97eb0c` → `e443a86e` | none (auto-merged; `cargo check` clean) | `decode.rs` +310/-11, `stream.rs` +239, `fixtures/444_rect_strip_leaf_tx_type.obu` (26839 B), `lanes/av1chrtx.report.md`, `lanes/av1readcensus.report.md` |
| 2 | `lane-av1skipfix` | `960be978` → `5e9c7715` | none | `refusal_inventory.rs`, `stream.rs` (902 chg), `ec-flac/tests/xiph_vectors.rs`, `ec-h264/tests/conformance.rs`, `ec-opus/tests/conformance.rs`, `scripts/link-fixtures.sh`, `lanes/av1skipfix.report.md`, `lanes/COMMON-20260901.md` |
| 3 | `lane-av1gates444` | `555c83f3` → `0eb19c11` | none | `stream.rs` +230, `lanes/av1gates444.report.md` |
| 4 | `lane-av1mutproof` | `ffd8d138` → `10db0dc2` | none — report-only lane | `lanes/av1mutproof.report.md` |
| 5 | `lane-av1toolgates` | `4c4bd3e2` → `962945ce` | none | `stream.rs` +332 |
| 6 | `lane-av1tilemeasure` | `df1370dd` → `b4c45166` | none | `stream.rs` +492, `lanes/av1tilemeasure.report.md` |
| 7 | `lane-av1superhbd` | `a2da03e4` → `e96de012` | none | 5 new pins (`…_d12_10bit` 15803 B, `_d12_12bit` 11023 B, `_d9_12bit` 13015 B, `…_mode2_…_10bit` 17776 B, `_12bit` 14656 B), `stream.rs` +409, `lanes/av1superhbd.report.md` |
| 8 | `lane-av1pins` | `44a05b40` → `7f8817cb` | **1 competing edit** in `stream.rs` — see below | 3 new pins (`golden3-pin.obu` 159 B, `ll444-lossless-key.obu` 7845 B, `sbpart-pin.obu` 238 B), `stream.rs` +243/-84, `lanes/av1pins.report.md` |
| 9 | `lane-av1distwtd` | `1aaf7e0b` → `a5867794` | **add/add at the tail of `mod tests`** — both sides kept, one closing brace re-inserted. Plus a **foreign worktree leak** caught mid-merge. | `decode.rs` +34, `gate_coverage.rs` +390/-40, `stream.rs` +226, `lanes/av1distwtd.report.md` |
| 10 | `lane-av1dualfilter10` | `c2b969c8` → `764ba257` | none (contains `1aaf7e0b`, so only its own delta lands) | `stream.rs` +92/-3, `lanes/av1dualfilter10.report.md` |
| 11 | `lane-av1lm444loss` | `9249442f` → `9a9e3674` | none — `decode.rs` regions are disjoint from `chrtx` and `distwtd` | `decode.rs` +81/-6, `stream.rs` +122, `lanes/av1lm444loss.report.md` |
| 12 | `lane-whtshape` | `f541d062` → `afa13bcf` | **2 interleaved add/add hunks** — resolved by construction, see below | `fixtures/ll444_sb64_1to4_lossless.obu` (51107 B), `decode.rs` +256/-45, `stream.rs` +200, `transform.rs` +223/-?, `lanes/whtshape.report.md` |
| 13 | `lane-av1superpin` | `9646f74d` → tip (see note) | **2 comment/add hunks in `decode.rs` (main's text kept), interleaved add/add in `stream.rs` (reconstructed), add/add in `lanes/av1formatsweep.report.md` (superpin + spliced row), one byte-identical duplicate gate deleted** — see below | 5 new pins, `decode.rs`, `stream.rs` +932 net, `lanes/av1444edge.report.md`, `lanes/av1formatsweep.report.md`, `lanes/av1superpin.report.md`, `gate_coverage.rs` (retirement reconciliation), `lanes/merge-wave-2.report.md` |

Row 13's merge commit cannot name its own hash inside itself; the authoritative
chain is `git log --first-parent 298f75c4..HEAD` and row 13 is the tip.

## Conflict resolutions, in detail

### 8 — `lane-av1pins` vs `lane-av1skipfix`: competing edits, not add/add

Both lanes rewrote the same arm of
`a_real_aomenc_lossless_444_key_frame_decodes_sample_exact`.

* ours (`skipfix`): read the pin from the **gitignored root** `fixtures/`
  through `pin_dir()`, with an opt-in `EC_REQUIRE_FIXTURES` /
  `EC_AV1_REQUIRE_AOMENC` hard fail.
* theirs (`pins`): read the pin that **this same merge commits**, through the
  new `crate_pin()`, but with a bare `SKIP` — i.e. exactly the
  report-green-having-decoded-nothing defect `skipfix` exists to close.

Resolution: **theirs' committed-pin path and provenance comment, plus ours'
no-silent-green intent made unconditional.** The committed pin is read through
`CARGO_MANIFEST_DIR` and a missing file panics with the regeneration recipe.
That is strictly stronger than the opt-in env fail — there is no environment
left to escape — and it is what `lane-av1skipfix`'s own later commit
`f385963f` independently concluded. Not `@both`: the two sides are the same
statement.

Marker surgery alone was not enough here: the hoisted shared `};` left an
orphan after the replacement, which surfaced as
`unexpected closing delimiter … 45608` (the marker-free-corruption shape).
Removed explicitly.

### 9 — `lane-av1distwtd`: add/add at the tail, plus a foreign leak

`stream.rs`: both lanes append `#[test]` fns at the end of `mod tests`, so this
is the classic add/add — **both sides kept**. Git hoisted the shared trailing
`    }` / `}` out of the hunk, so ours' last test needed its closing `    }`
re-inserted between the two blocks.

A **foreign uncommitted edit** appeared in the primary checkout while this merge
was in flight: 465 lines added to `gate_coverage.rs` carrying
`lane-av1txsearch`'s `bound_values` / `spelling_values` / `resolved_on_state` /
`is_ident_byte` — four functions this merge had just added, so the file
silently compiled to a **duplicated** set and failed with four borrow/type
errors and no conflict marker anywhere. It was preserved (byte copy below),
the merge result restored from the index, and `cargo check --all-targets`
re-run. Also present: an untracked `lanes/av1mergecheck.report.md` from a
sibling read-only review pass, which was swept out of this merge's tree
after an `--amend` and left untracked for its owner.

### 12 — `lane-whtshape`: two interleaved hunks

Both lanes append at the same tail anchor and git produced two conflict regions
that split **ours' last gate mid-statement** — the `<<<<<<<`/`=======` surgery
does not apply. Resolved by construction instead: this lane's `stream.rs`
delta against its merge base is a **single 199-line block appended at the
tail** (`@@ -43597,4 +43597,203 @@`, 199 added lines, 0 removed), so the merged
file is HEAD's `stream.rs` verbatim (its closing brace kept) + that block + the
module's closing brace. A first attempt stripped one brace too many and
produced an extra `}` at EOF; caught by `cargo check`, fixed.

### 13 — `lane-av1superpin`

* `decode.rs` (2 hunks, both `@ours`): one comment-only hunk where main's text
  is the more informative, and one where theirs simply has no counter —
  `INTRABC_RECT4_OWN_CHROMA444_HITS` is a main-only addition from an earlier
  lane that superpin's older base predates.
* `stream.rs`: same interleaved tail shape as #12. Superpin's delta against its
  merge base is 933 added lines at one hunk (plus **one** deleted blank line,
  cosmetic), so merged = HEAD verbatim + that block. That block re-adds
  `a_444_lossy_rect4_inter_stream_decodes_pixel_exact`, which superpin already
  carried from `e10a3b25` (`lane-av1444rect`) and main also has — a
  **byte-identical duplicate** (5905 B, extracted and compared, `IDENTICAL`).
  The appended copy was deleted; the gate is defined once. All six of
  superpin's new gates are present exactly once afterwards.
* `lanes/av1formatsweep.report.md` (add/add): **superpin's version**, with the
  **§6.2 `130x122` row spliced from `lane-park-av1444edge` @ `a02a2511`** — the
  correction that the cell is EXACT 4/4 (bytes 8945, sha `c87ac65b…`), the same
  H1 1:4-inter-strip class, closed by `lane-av1444rect`'s `f92776ba`, which is
  an ancestor of main. The original measurement text is **kept inside** the row
  (`was "DIVERGENT from f3, 11521 samples, first (f3, s2208) = Y(128,16)"; now
  EXACT 4/4`), not deleted. The rest of `a02a2511` was **not** merged.
* `gate_coverage.rs`: the wave-wide retirement reconciliation — see below.

### `gate_coverage.rs` — the union of retirements

Resolution applied across the wave, and the two lists are now **empty**, which
is what the census derives:

| entry | retired by | depth |
|-------|-----------|-------|
| `enable-dist-wtd-comp` | `a_distance_weighted_compound_stream_decodes_pixel_exact` (asserts the parsed `enable_jnt_comp`, and `dist_wtd_comp_hits() > 0`) | 8-bit |
| `enable-dual-filter` | `a_real_aomenc_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact` + `dual_filter_diff_hits()` arrival assert | 8-bit |
| `enable-dual-filter` | `a_real_aomenc_10bit_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact` (`lane-av1dualfilter10`) | 10-bit |
| `enable-flip-idtx` | `a_real_aomenc_stream_with_a_1d_tx_class_on_a_rect_transform_…` (`compared_class1 > 0`); detector fix — `covers_both_depths` now recognises the pair-of-pixel-formats marker | 8-bit |
| `enable-rect-tx` | `a_real_aomenc_{8,10}bit_stream_with_a_rect_transform_decodes_pixel_exact` (`lane-av1toolgates`); witness is the parsed shape (`rect_partition_hits` / `rect_coeff_tu_hits`), not the flag — the flag has no sequence-header bit | 8- and 10-bit |

`TOOL_UNIVERSE` keeps all 26 tools, `enable-rect-tx` included. The test was
**not** weakened. Measured before the edit (the real red, not an assumed one):

```
the 8-bit  list still names ["enable-rect-tx"], but a 8-bit gate now passes `=1` for them
the 10-bit list still names ["enable-dual-filter", "enable-rect-tx"], …
```

After: `print_never_exercised_per_bit_depth` reports **255 real-aomenc gate
bodies, NEVER_EXERCISED derived EMPTY at 8 bits and at 10 bits (0 of 26)**.

> A stale-binary trap worth naming: the first census run in this wave reported
> `145 gates / 4 of 26 / 2 of 26` and `gate_coverage` **passed 9/9**. That was a
> test binary built from a mid-reconstruction `stream.rs`, not the merged tree.
> `include_str!("stream.rs")` is compile-time, so a `cargo check` does not
> refresh it — only a real `cargo test` rebuild does. Re-running after
> `touch crates/ec-av1/src/gate_coverage.rs` gave 255 gates and the two genuine
> failures above.

## Duplicate fixtures

Measured after merge 13: `crates/ec-av1/fixtures/` holds **63 files, 63
distinct sha256 prefixes — zero duplicates**, so the deletion clause did not
fire. Checked in particular: superpin's `444_lossy_superres_256x128_d12.obu`
(14808 B, sha `450caa3e…`) versus `lane-park-av1444edge`'s rename target
`444_lossy_superres_random_256x128.obu` (sha `c18496cf…`) — different bytes,
and the rename lives only in the unmerged `a02a2511`, so nothing collides.

## Verification actually run

```
cargo check -p ec-av1                  after EVERY merge    → clean
cargo check -p ec-av1 --all-targets    after EVERY merge    → clean
bash /tmp/losscheck.sh <pre> <parentA> <parentB>            → LOST empty at EVERY merge
cargo test -p ec-av1 --lib -- gate_coverage refusal_inventory
    → test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 717 filtered out
```

`losscheck.sh` diffs the `fn a_*` set of the merged `stream.rs` against the
union of both parents' sets, in **both** directions, after every merge. Final
state: `now=285 union=285`, LOST empty, ADDED empty.

`gate_coverage` and `refusal_inventory` are both GREEN on the merged tree, and
the census is green because the entries were retired, not because the test was
touched. Both `never_exercised_*bit_matches_the_gate_recipes` failures quoted
above were observed as real reds immediately before the retirement edit.

## Post-merge fix tasks (NOT fixed here)

1. **`lane-av1gates444`'s three gates are green on a host with no oracle.**
   `a_444_lossy_odd_66x66_…`, `a_444_lossy_odd_130x122_…` and
   `a_444_lossy_256x128_…` share the arm
   `if !have_aomenc() { eprintln!("SKIP {name}: no aomenc oracle at {} …"); return Vec::new(); }`
   in the shared helper `encode_444_lossy_live`. On a host without
   `aomenc`/`aomdec` all three return `rc=0` having measured nothing — the
   class `gate-blind-to-feature` in its purest form. They need the
   `EC_AV1_REQUIRE_AOMENC` hard-fail shape the rest of the crate already uses.
2. **`refusal_inventory.rs`'s `gates_that_swallow_a_decode_error_are_declared`
   is blind to 354 of 397 arms.** It filters parsed gate names with
   `chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')`,
   so every arm that emits the interpolated `eprintln!("SKIP {NAME}: {e}")` is
   exempt while only the literal-lowercase arms are enforced. The guard must
   see the lanes' own new arm shape (the interpolated one), not just the 43
   literal spellings.

## Left behind on purpose

* Branch heads past the dispatched tips, **not merged** (state, not a
  recommendation): `lane-av1chrtx` `6cc9ea6f` (report/branch-name + gate-name
  fix), `lane-av1skipfix` `b696d485` (commits the 444 lossless key-frame pin at
  7851 B and rewires that gate onto `CARGO_MANIFEST_DIR` — note this is a
  **different regeneration** from the 7845 B pin `lane-av1pins` committed, so
  merging both would put two same-recipe pins in the crate),
  `lane-av1lm444loss` `5519aaf8` (ss-aware chroma geometry + lossless TX_4X4
  raster — the follow-on lead (b) the report's appendix names as open).
* `lane-park-av1444edge` @ `a02a2511`: only the §6.2 row was taken. Its
  remaining r4 work (`84846129`'s `a_444_superres_arm` gates, superseded by
  superpin's; `bb44ddd0`'s sweep corrections) is **not** merged.
* Foreign files preserved, not committed:
  `~/.cache/merge-wave-2-keep/gate_coverage.rs.lane-av1txsearch-leak` and
  `~/.cache/merge-wave-2-keep/av1mergecheck.report.md.foreign`. The latter is
  still untracked in the worktree for its owner.

**No push was performed.** `main` is ready to push from `298f75c4` + these 13
merge commits.
