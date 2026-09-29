# lanes/av1w3erefute — wave-3e refutation, read-only

Worktree `/home/tahinli/.cache/wt/av1w3erefute`, branch `lane-av1w3erefute`, base `3b691e13`
(main). `CARGO_TARGET_DIR=/home/tahinli/.cache/wt/tgt-av1w3erefute`. Oracle:
`~/.cache/aom-oracle/build/{aomenc,aomdec}`.

Note on provenance: the tip sha `4bfe8d8e` is NOT an ancestor of main
(`git merge-base --is-ancestor 4bfe8d8e main` → NO). The *content* landed
squashed/rebased through `c0727d64` inside `lane-av1txsizeaudit-r5`; all three
items below were located in main's tree, not in the foreign branch's.

## Verdict table

| # | Item | Verdict | Deciding raw line |
|---|------|---------|-------------------|
| 1 | `4bfe8d8e` intra-in-inter fix, gate `stream::tests::a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact` | **PASS** | `3 decode-order frames pixel-exact (0 hidden), intra_128_in_inter=1 mu_chunk_chroma_reads=4` |
| 2 | `C1` `fctx` into `suppress_internal_lf_edges` + source-scan gate | **PASS** (gate), with one dead assertion (see Not-done) | `suppress_internal_lf_edges must publish the block's own chroma extent per axis ... not a hardcoded `/2`` |
| 3 | `pins5` generalised count/pin guard, ceiling 43 / floor 40 | **PASS** | `count-vacuity sweep: 60 site(s); 17 spec-pinned by a prior assert, 43 not` |

---

## Item 1 — intra-in-inter 128-root mu-chunk chroma gate

### Exists, runs, cannot skip

`crates/ec-av1/src/stream.rs:37628` — `#[test] fn
a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact()`.

Non-vacuity is asserted three ways in the gate itself, not merely "ok":
sequence header asserted 4:4:4/128-SB, `intra_128_in_inter_hits() >= 1`,
`intra_128_in_inter_mu_chroma_hits() >= 4`, plus byte-exact compare per frame.

```
$ EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -- --nocapture --test-threads=1 \
    a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact
running 1 test
test stream::tests::... ... a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact: 3 decode-order frames pixel-exact (0 hidden), intra_128_in_inter=1 mu_chunk_chroma_reads=4
ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 761 filtered out; finished in 0.53s
```

Route counters are `> 0` (1 root, 4 mu-chunk chroma reads) — the arm ran.

### The env really reaches the test (it cannot report green off a SKIP)

`stream.rs:9404` `have_aomenc()` asserts when `EC_AV1_REQUIRE_AOMENC` is set;
`stream.rs:5283` `have_ffmpeg()` asserts the same for the ffmpeg half.

```
$ EC_AV1_AOMENC=/nonexistent/aomenc EC_AV1_REQUIRE_AOMENC=1 cargo test ... <gate>
EC_AV1_REQUIRE_AOMENC is set but no aomenc at /nonexistent/aomenc -- run scripts/build-aom-oracle.sh
test result: FAILED. 0 passed; 1 failed; ...          # env reached the test

$ EC_AV1_AOMENC=/nonexistent/aomenc cargo test ... <gate>     # env unset
test ... SKIP a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact: no ffmpeg/aomenc/aomdec
test result: ok. 1 passed; ...                        # the developer escape, and only this one
```

### Mutation proof (RED → restored GREEN)

Guarded value: the per-mu-chunk CHROMA extent at `decode.rs:43262-43263`, mutated from the
ss-aware form back to the 4:2:0-hardcoded literal.

```diff
  crates/ec-av1/src/decode.rs
  let chunk_luma = 64usize;
- let chunk_chroma_w = chunk_luma >> ss_x(fctx);
- let chunk_chroma_h = chunk_luma >> ss_y(fctx);
+ let chunk_chroma_w = 32usize;
+ let chunk_chroma_h = 32usize;
```

RED:

```
$ EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -- --nocapture --test-threads=1 <gate>
thread '...pixel_exact' panicked at crates/ec-av1/src/stream.rs:9683:17:
a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact: decode-order frame 2 of 3 (3 shown, 0 hidden) differs from the oracle at byte 15450 (ours 142 vs 141), 44649 bytes differ
test result: FAILED. 0 passed; 1 failed; ...
```

The 44649-byte figure matches the number the gate's own doc comment claims
(`stream.rs:37620-37623`) — the claimed red-before is real, and the red is on
PIXELS, not on the counter.

Restore (`git checkout -- crates/ec-av1/src/decode.rs`, `git status --short` empty):

```
a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact: 3 decode-order frames pixel-exact (0 hidden), intra_128_in_inter=1 mu_chunk_chroma_reads=4
ok
test result: ok. 1 passed; 0 failed; ...
```

**Item 1: PASS.**

---

## Item 2 — `C1`, `fctx` threaded into `suppress_internal_lf_edges`

Gate: `refusal_inventory::tests::the_skipped_128_root_chroma_suppression_publishes_the_blocks_own_per_axis_chroma_extent`
(`crates/ec-av1/src/refusal_inventory.rs:1862`).

Baseline:

```
$ cargo test -p ec-av1 --lib -- --test-threads=1 the_skipped_128_root_...extent
test refusal_inventory::tests::... ... ok
test result: ok. 1 passed; 0 failed; ...
```

### Mutation A — guarded expression

```diff
  crates/ec-av1/src/decode.rs:9279
- let (uv_w, uv_h) = (((w_mi * MI) >> ss_x(fctx)).max(4) as u8, ((h_mi * MI) >> ss_y(fctx)).max(4) as u8);
+ let (uv_w, uv_h) = ((w_mi * MI / 2).max(4) as u8, (h_mi * MI / 2).max(4) as u8);
```

RED:

```
suppress_internal_lf_edges must publish the block's own chroma extent per axis -- `w_mi * MI >> ss_x(fctx)`, not a hardcoded `/2`
test result: FAILED. 0 passed; 1 failed; ...
```

### Mutation A also proves the scan reads the BODY, not a same-indent window / EOF

`decode.rs:9301` — the *sibling* `fill_lf_grid_rect` — still carries the identical
correct string `((w_mi * MI) >> ss_x(fctx))`, and it sits 22 lines BELOW
`suppress_internal_lf_edges`'s closing brace. If the `&src[body_start..body_start +
src[body_start..].find("\n    }")]` slice over-ran (to EOF, or past the function),
the positive `body.contains(...)` would have been satisfied by the sibling and the
gate would have stayed green. It went RED. The window is bounded at the function's
own 4-space closing brace (`decode.rs:9285`), and the `stale` loop
(`"w_mi * MI / 2"`, `"h_mi * MI / 2"`) is inside the same window.

Restore: `git checkout -- crates/ec-av1/src/decode.rs` → gate `ok` again.

### Is the change observable on any committed fixture?

No. Full mutation (`w_mi * MI / 2`, the pre-`C1` form) over the whole committed
4:4:4 corpus:

```
$ EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib 444
...
test result: ok. 38 passed; 0 failed; 0 ignored; 0 measured; 724 filtered out; finished in 22.70s
```

38/38 pixel-exact gates stay green with the fix reverted, including
`a_real_aomenc_444_intra_in_inter_128_root_codes_chroma_per_mu_chunk_unit_pixel_exact`
and `a_444_sb128_root_rect_stream_with_restoration_decodes_pixel_exact`. 4:2:0 is
unaffected by construction (`>> 1 == / 2`).

#### What is REPRODUCED here vs what is QUOTED from the author

| Claim | Status |
|---|---|
| Reverting `suppress_internal_lf_edges` to `w_mi * MI / 2` changes no committed 4:4:4 pixel (38/38 green) | **REPRODUCED here** — command and output above |
| The source-scan gate goes RED on exactly that revert, and green again on restore | **REPRODUCED here** — red/green pair above |
| The source-scan reads the function BODY, not a same-indent run to EOF | **REPRODUCED here** — the sibling at `decode.rs:9301` carries the identical correct string and the mutated gate still red |
| "30 fires / 2 555 904 samples / 26 frames across five 4:4:4 `--sb-size=128` streams" | **QUOTED, NOT REPRODUCED** — author-reported (`refusal_inventory.rs:1852-1858`). I did not re-run those five streams or re-count the arm firings. My evidence is the WEAKER claim in the first row, and it is sufficient for the verdict. |
| "no pixel gate can exist for this site / the masking is TOTAL" | **PARTLY REPRODUCED** — no committed 4:4:4 fixture observes the value, consistent with it. The universal statement (no possible input distinguishes 64 from 128 here) is quoted, not proven. |

**Item 2: PASS** on the gate's existence, non-vacuity, body-scoping, and
correct restoration. Caveat kept: the third assert (`sig_end` contains
`fctx: &FrameCtx`) can never be the deciding assertion — the positive body
assert reds first. Defense-in-depth, not a failure.

---

## Item 3 — `pins5` count/pin guard

Gates: `gate_coverage::count_vacuity_tests::the_count_vacuity_sweep_finds_the_known_sites`
(floor 40 + ceiling 43, `gate_coverage.rs:2374-2427`) and
`...::every_locally_derived_oracle_count_is_reported` (the table).

Baseline:

```
$ cargo test -p ec-av1 --lib count_vacuity -- --nocapture --test-threads=1
count-vacuity sweep: 60 site(s); 17 spec-pinned by a prior assert, 43 not
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 760 filtered out; finished in 0.08s
```

Arithmetic checks out: 17 + 43 = 60 exactly, and the 43 reported lines
(`NOT SPEC-PINNED`) counted by grep = 43. The reported sweep line, the ceiling
constant and the unpinned population are one number, not three.

### Mutation A — drop one site ⇒ the tracked count falls 43 → 42

```diff
  crates/ec-av1/src/stream.rs:5631
- let want = ffmpeg_decode_sequence(&data, width, height, decoded.len());
+ let want = ffmpeg_decode_sequence(&data, width, height, decoded.len() + 0);
```

```
count-vacuity sweep: 59 site(s); 17 spec-pinned by a prior assert, 42 not
test result: ok. 2 passed; 0 failed; ...
```

The site set, the pinned count and the unpinned count all move together. With the
ceiling lowered to 41 the same tree reds, naming the LIVE count:

```
42 unpinned count site(s), ceiling is 41 -- a new one was added (lower the ceiling only when a site is actually fixed)
test result: FAILED. 0 passed; 1 failed; ...
```

So the "correct ceiling" after that fix is 42, one lower than 43 — removing a site
moves the ceiling DOWN, as required. (The guard deliberately does not force the
constant down; the constant is only a "no new site" tripwire.)

### Mutation B — a new unpinned site ⇒ immediate red at the real ceiling 43

Narrowed the pin-detection window (`gate_coverage.rs:2209`) from `1..=14` to
`1..=1` so five previously-pinned sites read as unpinned; ceiling restored to 43:

```
count-vacuity sweep: 60 site(s); 12 spec-pinned by a prior assert, 48 not
48 unpinned count site(s), ceiling is 43 -- a new one was added (lower the ceiling only when a site is actually fixed)
test result: FAILED. 1 passed; 1 failed; ...
```

### Mutation C — the floor is live

Dropped `"ffmpeg_decode_sequence("` from the scanner's callee list
(`gate_coverage.rs:2167`):

```
count-vacuity sweep: 25 site(s); 12 spec-pinned by a prior assert, 13 not
the count-vacuity sweep found only 25 site(s); expected 40+ -- it has lost a shape
test result: FAILED. 1 passed; 1 failed; ...
```

Restore: `git checkout -- crates/ec-av1/src/gate_coverage.rs crates/ec-av1/src/decode.rs
crates/ec-av1/src/stream.rs`; `git status --short` empty; all three gates green
again (`60 site(s); 17 spec-pinned ... 43 not`, `1 passed` for each of the two
pixel/source gates).

**Item 3: PASS.**

### Class sweep — unexercised matcher in a source scanner

My first pass called `"ffmpeg_decode_sequence_444("` a dead matcher. **That was
wrong, and the correction is the interesting part.** Deleting it left the sweep at
`60/17/43` because it produces no count-vacuity SITE — but it is not rot: it fires
on five real `stream.rs` gates (`stream.rs:7030, 7545, 7735, 8439, 11643`), each
of which passes the spec const `FRAMES` rather than a decode-derived `.len()`, so
the `.len()`/`.count()` filter correctly rejects all five. **KEPT, with a reason**,
and the control measures the SPELLING (≥1 line uses it) rather than the produced
site — measuring the site is what made it look dead in the first place.

So the real defect is one level up: the sweep's floor catches a LOST shape, and
nothing caught a matcher that never fired. Every matcher in the scanner was
measured. Nine of them fire on nothing anywhere in the crate:

| Matcher | Table | Decision | Evidence |
|---|---|---|---|
| `ffmpeg_decode_sequence(` | callee | keep (live) | 174 lines; 35 of the 60 sites |
| `ffmpeg_decode_sequence_10bit(` | callee | keep (live) | 87 lines; 25 of the 60 sites |
| `ffmpeg_decode_sequence_444(` | callee | **keep + reason** | 6 lines, 5 call sites, 0 sites by construction — see above |
| `census`/`probe`/`sweep`/`first_diff`/`scratch`/`rectx` | probe substrings | keep (live) | 9/2/4/1/2/1 fns |
| `diagnostic` | probe substrings | **DELETE** | 0 fns; `grep -rn '\bdiagnostic\b' crates/ec-av1/src` hits only prose in other files, never a fn name |
| `encode_aomenc_stream`, `screen_intrabc_stream_at_depth`, `screen_intrabc_stream_with`, `libaom_encode_with`, `libaom_encode`, `run_multi_tile_gate`, `edge32_gate`, `rect_tx_tool_gate`, `warp_gate` | enc helpers | keep (live) | 3/3/5/5/2/8/4/3/5 fn bodies |
| `the_chroma_rect_gates`, `restore_gate`, `cdef_gate`, `superres_gate`, `grain_gate`, `sgate` | enc helpers | **DELETE (6)** | 0 fn bodies; `grep -rn '\bNAME\b' crates/ec-av1/src` returns the matcher list and NOTHING ELSE — no helper, no caller, no comment |
| `FRAMES` / `frame_count` | spec bindings | keep (live) | 24 / 7 fn bodies |
| `NFRAMES`, `frames_expected` | spec bindings | **DELETE (2)** | 0 fn bodies; occur nowhere else in the crate |
| `aomenc_path()`, `fs::read`, `crate_pin` | bucket leaves | keep (live) | 188 / 71 / 9 fn bodies |
| pin-detection window (14) | `for back in 1..=14` | keep (live) | pins 17 of 60 |

**Nine dead spellings, all deleted** (1 probe substring + 6 encode helpers + 2 spec
bindings). Every one was provably rot — the string appears nowhere in the crate
outside the matcher table itself, so there is no hypothetical caller to name and
nothing loses coverage by removing it. Per the lane's stop rule I went no further:
no behavioural change to the scanner beyond the deletions, and no re-design of the
classifier.

### The positive control, and its own mutation proof

All the tables are now single-sourced `pub const`s (`CALLEES`, `PROBE_SUBSTRINGS`,
`ENC_HELPERS`, `FIXABLE_CANDS`, `AOMENC_LEAF`, `DISK_READ_LEAVES`, `PIN_WINDOW`) and
the scanner reads those same consts, so a matcher added later cannot skip its
control. The control is
`gate_coverage::count_vacuity::every_scanner_matcher_fires_on_a_known_site`
(`gate_coverage.rs:2467`), and it measures "fires" on the EXACT `fn` body slice the
scanner classifies with — the brace walk was extracted from `sites` into
`count_vacuity::fn_body_end` so the control cannot drift onto a different slice
(class: control-and-measurement-drift).

Green after the change:

```
$ cargo test -p ec-av1 --lib -- --nocapture --test-threads=1 count_vacuity
test gate_coverage::count_vacuity::every_scanner_matcher_fires_on_a_known_site ... ok
test gate_coverage::count_vacuity_tests::every_locally_derived_oracle_count_is_reported ... count-vacuity sweep: 60 site(s); 17 spec-pinned by a prior assert, 43 not
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 760 filtered out; finished in 0.13s
```

The sweep line is UNCHANGED at **60 / 17 / 43** after the nine deletions, which is
the proof the deletions cost no coverage.

RED — one of the deleted spellings re-added (`ENC_HELPERS += "sgate"`):

```
thread 'gate_coverage::count_vacuity::every_scanner_matcher_fires_on_a_known_site' panicked at crates/ec-av1/src/gate_coverage.rs:2505:13:
the encode-helper matcher `sgate(` matches NO fn body in stream.rs -- it is dead coverage (grep the crate: if the helper has no caller, delete the entry; if it has a caller outside stream.rs, say so here and name the file)
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 762 filtered out; finished in 0.04s
```

Restored: the entry is cut again and the control is `ok`.

---

## Declared but unproven / not done

1. **The `sig_end` assertion in the C1 gate is unreachable as a deciding
   assertion.** The positive `body.contains("((w_mi * MI) >> ss_x(fctx))")` assert
   runs first, so any edit that drops `fctx: &FrameCtx` from the signature must
   also change the body, and the body assert reds first. Not a defect (the code
   would not compile without the parameter, which the comment states correctly),
   but the third assert is defense-in-depth that never fires on its own.
2. **C1's "30 fires / 2 555 904 samples over 26 frames across five 4:4:4
   `--sb-size=128` streams" is AUTHOR-REPORTED and NOT REPRODUCED here.** What I
   verified is weaker and stated precisely: reverting the value changes no
   committed 4:4:4 pixel (38/38 green), and the source scan's red on exactly that
   revert is mutation-proven. See the "REPRODUCED here vs QUOTED from the author"
   table under Item 2.
3. **No per-route counter exists for `suppress_internal_lf_edges`**, so item 2's
   non-vacuity rests entirely on the source scan plus the negative pixel
   evidence. That is consistent with the gate's own argument, but it means no
   positive "this arm fired" witness exists for the site.
4. **No full suite was run** (per lane contract; heavy runs belong to the VPS
   fleet). Every run here was `cargo test -p ec-av1 --lib <filter>`, on a
   `cargo check`-clean tree, with a per-lane `CARGO_TARGET_DIR`.
5. **The `main`-vs-`4bfe8d8e` provenance** is by content equivalence at
   `3b691e13`, not by ancestry: the sha is not in main's history.
6. **Decoder code was temporarily mutated** in this worktree for the red/green
   pairs above and restored via `git checkout --` after each. The only PERMANENT
   change is `crates/ec-av1/src/gate_coverage.rs`: the nine dead matcher
   deletions, the const single-sourcing, the extracted `fn_body_end`, and the new
   positive-control test. The sweep is UNCHANGED at `60 site(s); 17 spec-pinned,
   43 not`. No guard was relaxed; no decoder change is proposed.
