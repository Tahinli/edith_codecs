# lane-av1pinspec2 — commit the SVT crop, count off the wire, and make the sweep comment-aware

Branch `lane-av1pinspec2`, worktree `~/.cache/wt/av1pinspec2`, base `2c3d3b78` (main
with lane-av1pinspec merged). **Two commits, and the ledger is empty.**

```
e923b420 gate_coverage: make the count-vacuity scan comment-aware, ceiling 1 -> 0
37059b34 stream: run the SVT split-transform gate on a committed pin, count off the wire
```

**Outcome (a) on the branch point Main asked me to decide: the SVT crop IS
committable, and it is committed.** A bounded search found a byte-reproducible
`-frames:v 1` libsvtav1 encode that reaches the fixed arm and is pixel-exact, so
the gate now runs on a stock box and its count is derived from the wire. The
alternative (leave the site, name it as an exception) was not needed.

## 0. The sweep line, before and after

```
before:  count-vacuity sweep: 59 site(s); 58 spec-pinned by a prior assert, 1 not
after:   count-vacuity sweep: 58 site(s); 58 spec-pinned by a prior assert, 0 not
```

`59 -> 58` is the SVT gate leaving the population: it no longer spends a
decode-derived `.len()` at all, so it is not a site. `1 -> 0` is the doc-comment
row ceasing to be a site. Ceiling `2 -> 1 -> 0`, tied to each commit's measured
population.

## 1. Commit 1 — the SVT crop is committable, and here it is

### 1a. Why lane-svt1's own search missed, and the one parameter that fixes it

`lanes/svt1.report.md` records a 6 sources x 5 sizes x 6 crf sweep that produced 8
streams reaching the fixed arm, and killed every one of them: 2 refused on an
unrelated Golomb tail, 6 missed by ~200k-245k samples **on unrelated inter-frame
defects**. That search ran **FOUR frames per stream**. A single key frame has no
inter frames, which is the entire difference.

The search here: 7 sources x 6 sizes x 6 crf, `-frames:v 1`, `r=12`,
`screen-content-mode=1`, measuring for each candidate that reached the fixed arm
(`skip_split_tx_override_hits > 0`) whether it also decoded and compared exact:

```
SEARCH DONE: 1 stream(s) reached the fixed arm, 1 exact
CANDIDATE hits=1 frames=1 192x152 crf=50 bad=0 bytes=1838 :: smptehdbars=s={w}x{h}:r=12,drawgrid=w=8:h=8:t=1:c=black
*** EXACT WINNER
```

The six `crf=70` cells are the one dead spot in the sweep and are recorded here
rather than hidden: `libsvtav1` rejects it (`Value 70.000000 for parameter 'crf'
out of range [0 - 63]`).

### 1b. The pin, and its provenance

| path | sha256 | size |
|---|---|---|
| `crates/ec-av1/fixtures/svt1-split-tx-pin.obu` | `c9d9c0ae934fba72fc9a9374bc36853f290796a8a4d6442140e239669e0b341f` | 1838 B |

**A REPLAY, not a fresh capture**, and that is the stronger property: two
independent encodes of

```
ffmpeg -v error -f lavfi \
  -i "smptehdbars=s=192x152:r=12,drawgrid=w=8:h=8:t=1:c=black" \
  -frames:v 1 -c:v libsvtav1 -preset 8 -crf 50 \
  -svtav1-params "lp=1:screen-content-mode=1" -g 12 -f obu out.obu
```

(SVT-AV1 v3.1.2) produce those exact bytes:

```
c9d9c0ae934fba72fc9a9374bc36853f290796a8a4d6442140e239669e0b341f  /tmp/svt1-repro1.obu
c9d9c0ae934fba72fc9a9374bc36853f290796a8a4d6442140e239669e0b341f  /tmp/svt1-repro2.obu
c9d9c0ae934fba72fc9a9374bc36853f290796a8a4d6442140e239669e0b341f  /tmp/svt1-win-192x152-crf50.obu
```

Committed with `git add -f` — `.gitignore:2` is a bare `fixtures`, which git
matches at any depth, so the crate's own fixture directory is ignored too.

### 1c. The count, and the `show_existing_frame` question answered with evidence

The gate parses the pin's own OBUs with `Av1Parser` and asserts
`decoded.len() == frame_headers` **before** handing that number to the oracle —
the same wire-derived shape lane-av1pinspec used at `stream.rs:13267` for the
cdf-update gate, so there is now one idiom for "the count belongs to the wire"
rather than two.

A header count would **over-count** a stream carrying `show_existing_frame`
(spec 5.9.5: it codes no pixels, so the header count exceeds the shown-frame
count), which is exactly the objection that made this a hand-forward last round.
It is settled by assertion rather than by argument — the loop **refuses** any
frame header with `show_existing_frame` set:

```rust
assert!(
    !header.show_existing_frame,
    "{NAME}: frame header {frame_headers} sets show_existing_frame, so the header \
     count is not the shown-frame count and this gate's oracle budget would be wrong"
);
```

so "one header == one shown frame" is a checked fact on this pin, and a future
pin that does carry one reds with the reason instead of silently skewing the
oracle budget.

### 1d. Non-vacuity, measured, not asserted

* The gate **runs on a stock box** — `EC_AV1_SVT1_STREAM` unset, `ok` in 0.18 s,
  **no SKIP line**. That was the whole reason for the hand-forward.
* The premise bites: `skip_split_tx_override_hits() > 0` mutated to `> 1` reds,
  so the fixed arm fires **exactly once** on this pin and the gate cannot pass
  blind.

```
$ cargo test -p ec-av1 --lib -- an_svt_screen_palette_block_with_a_split_transform_decodes_exactly
an_svt_...: no skipped split-transform palette block in
  .../crates/ec-av1/fixtures/svt1-split-tx-pin.obu -- the gate would pass blind
test result: FAILED. 0 passed; 1 failed
```

### 1e. Red-before for the commit

Revert `stream.rs`, keep the new ceiling of 1:

```
$ git checkout HEAD -- crates/ec-av1/src/stream.rs
$ cargo test -p ec-av1 --lib -- count_vacuity_tests
2 unpinned count site(s), ceiling is 1 -- a new one was added (lower the ceiling only when a site is actually fixed)
test result: FAILED. 1 passed; 1 failed
```

And the new assert bites:

```
$ # frame_headers -> frame_headers + 1
assertion `left == right` failed: an_svt_...: the stream codes 1 frame(s), the decode showed 1
  left: 1
 right: 2
test result: FAILED. 0 passed; 1 failed
```

The "decode showed 1" is the proof the wire count and the decode agree on this
pin, not merely that an assert exists.

## 2. Commit 2 — the sweep is comment-aware

The last unpinned row was not a count. It was a `///` doc comment quoting the
pre-r4 body of `pinned_lr_sgr_stream_call_unique_dump` — prose about code,
sitting inside the gate the prose describes, which already spends `FRAMES` and
asserts `pics.len() == FRAMES`. A line-based scan cannot tell the two apart.

`sites()` now skips lines whose trimmed start is `//`. **Only the SITE scan
skips them**: the pin-window walk still sees every line, because a `//`-led line
there is a comment *between* an assert and its call, not a site.

### 2a. The capability test, both halves

`count_vacuity_tests::a_doc_commented_count_call_is_not_a_site_but_real_code_still_is`,
over a **SYNTHETIC** source rather than the live tree — a fix to the very gate
that motivated this would otherwise delete the row that proves the scanner works,
which is the failure mode `pin_inventory` already records as "a scanner that
cannot see this shape is how 14 pins survived three rounds".

* **half one** — a count call inside a `///` doc comment is NOT a site. This is
  the real row this change removes.
* **half two, the control** — the *same call as real code* IS still a site, is
  attributed to the right gate, and is still **pinned** by an assert above it.
  This is the half that matters: a scanner that skips comments by skipping the
  wrong thing looks identical on half one, and a ceiling of 0 is exactly the
  state where an over-broad skip would go unnoticed.

### 2b. Red-before for the commit

The `t.starts_with("//")` guard removed, ceiling at 0 — **both halves red**:

```
$ cargo test -p ec-av1 --lib -- count_vacuity_tests
test ...::a_doc_commented_count_call_is_not_a_site_but_real_code_still_is ... FAILED
test ...::the_count_vacuity_sweep_finds_the_known_sites ... FAILED
1 unpinned count site(s), ceiling is 0 -- a new one was added (lower the ceiling only when a site is actually fixed)
test result: FAILED. 1 passed; 2 failed
```

The first half reds because the doc-comment site is counted again; the second
because the population is 1 above a ceiling of 0. Neither half can be reverted
silently.

## 3. Green

```
$ cargo check -p ec-av1 --all-targets
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.71s

$ cargo test -p ec-av1 --lib -- gate_coverage pin_inventory count_vacuity
test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 744 filtered out; finished in 0.96s

$ cargo test -p ec-av1 --lib -- count_vacuity_tests::every_locally_derived_oracle_count_is_reported --nocapture
count-vacuity sweep: 58 site(s); 58 spec-pinned by a prior assert, 0 not
```

`every_pin_a_gate_reads_is_committed_under_the_crate` passes **with the new pin in
its scope**: `crate_pin` is called with a string **literal** on purpose, because
the pin inventory matches a pin read by the quote that follows the callee name —
a `const PIN` argument would have left this pin outside that invariant. That
call is also why the comment above it must not quote the spelling: the scanner
reads prose too, and a quoted spelling there parsed as a pin called
`` ` and would not see a const argument, ... `` and failed the invariant. Both
halves of that were found by running the invariant, not by reading the code.

## 4. Not done / deferred

Nothing is deferred. The ledger is empty (`0 not`), and the two rows lane-av1pinspec
handed forward are both closed — one by committing the crop, one by teaching the
scanner what prose is.
- No full `cargo test -p ec-av1` run — project-wide validation is Main's.
- The `crf=70` column of the search is out of `libsvtav1`'s range and is recorded
  as a dead cell rather than dropped; it does not affect the winner, which is
  `crf=50`.
- The committed pin is 8-bit 4:2:0 at 192x152. It is a **key frame only**, so this
  gate says nothing about SVT's inter-frame coding — that is the defect surface
  `lanes/svt1.report.md` measured and this crate still does not decode, and it is
  untouched by this lane.
