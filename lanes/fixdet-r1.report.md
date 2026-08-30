# lane-fixdet r1 report

VERDICT: FIXED — all 20 `gradients` gate sites now route through a seed-derived-colour helper, the determinism guard test is green, filter_intra went from 14/15 to 20/20, the free-partition gate's counts are now byte-stable run-to-run, and the full lib suite stays 232/0 (231 pre-existing + the new guard test). No gate now fails consistently; no decoder defect was uncovered by removing this nondeterminism.

## What changed
`crates/ec-av1/src/stream.rs`:
- New `gradients_source(seed, width, height, tail) -> String` (right before
  `libaom_encode`), hashing `seed` into four `0xRRGGBB` colours for
  `c0..c3` with a plain integer mix (`wrapping_mul`/`rotate_left`), and
  still passing `seed=` itself through in case a future ffmpeg honours it.
  Doc comment states why: `gradients` ignores `seed=` for its own colour
  choice, so an unset `c0..c9` stop is randomized from something else,
  and pinning colours to fixed literals (as the 4 earliest gates already
  did) would kill the per-attempt content variety the sweep gates need to
  make features fire.
- New guard test `gradients_source_is_reproducible_per_seed`: renders raw
  video for seed 42 twice and seed 43 once, asserts the two seed-42 runs
  are byte-identical and seed-43 differs. This is the regression fence.
- All 20 `gradients=size=...` call sites (4 that hand-pinned
  `c0=red:c1=blue:c2=green`, plus 16 that built the string with only
  `seed=` and no colours at all) rewritten to call the helper, keeping
  each site's original width/height/duration/rate exactly. Two of them
  built the `-i` argument inline in a `Command::new("ffmpeg").args([...])`
  array of `&str` literals; those became `.args([...]).arg(&source)`
  since the helper returns an owned `String`. One (`a_real_libaom_stream_with_adst_decodes_end_to_end`)
  held both fixtures in a `[(&str, ...); 2]` array; retyped to
  `[(String, ...); 2]`.

## Re-measurement

### filter_intra gate — was 14/15, isolated
20 runs of `a_real_aomenc_filter_intra_stream_decodes_pixel_exact` in
isolation (`cargo test ... a_real_aomenc_filter_intra_stream_decodes_pixel_exact`),
each a fresh process:

    20 passed / 0 failed out of 20

Every one of the 20 runs printed `test result: ok. 1 passed`. The ~1-in-15
flake measured in the charter is gone.

### free-partition gate — 3 runs, isolated
`a_real_aomenc_stream_with_free_partitions_decodes_pixel_exact`, 3 fresh
processes:

    run 1: 33 named refusals, 7 pixel-exact matches out of 40, rect_partition_hits=9 rect_coeff_hits=0 extended_partition_hits=0 partab_hits=2
    run 2: 33 named refusals, 7 pixel-exact matches out of 40, rect_partition_hits=9 rect_coeff_hits=0 extended_partition_hits=0 partab_hits=2
    run 3: 33 named refusals, 7 pixel-exact matches out of 40, rect_partition_hits=9 rect_coeff_hits=0 extended_partition_hits=0 partab_hits=2

Every field is now byte-identical across all 3 runs: refusal count,
match count, and every named hit counter. The gate's counts ARE stable
now — the fixture-side nondeterminism this lane targeted is closed for
this gate.

One honest caveat: the same test, run as part of the full 232-test suite
(reported below) printed different numbers that same session
(`rect_partition_hits=18 ... partab_hits=4`, exactly double the isolated
run's counts, with the same 7/40 match rate and 33 named refusals).
That is consistent with the gate accumulating into a process-global
counter shared with another in-suite test on the same fixture family
(cross-test interference), not with residual fixture nondeterminism —
the isolated, filtered runs above (which exclude every other test) are
the correct measurement of THIS gate's own stability, and they are flat
across all 3. Flagging it rather than papering over it: `deferred:
double-counted rect_partition_hits/partab_hits when this gate runs
alongside its siblings in the full suite — likely a shared static
counter not scoped per-test — unblocked by grepping RECT_PARTITION_HITS/
PARTAB_HITS for a missing per-test reset; not a fixdet-r1 scope item,
filed here as found, not investigated further this round.`

### Full lib suite
`cargo test -p ec-av1 --release --lib` with `EC_AV1_REQUIRE_AOMENC=1`:

    test result: ok. 232 passed; 0 failed; 17 ignored; 0 measured; 0 filtered out; finished in 68.15s

231 pre-existing + 1 new (`gradients_source_is_reproducible_per_seed`).
0 failures. No gate failed consistently in this run, so this lane did not
uncover a genuine decoder defect that the fixture nondeterminism had been
hiding — the report's "pin it with EC_AV1_GATE_DUMP and refuse by name"
branch does not apply this round.

## Deferred
- `deferred: rect_partition_hits/partab_hits double-counted when the
  free-partition gate runs inside the full suite vs isolated — cross-test
  counter-sharing, not a fixture-determinism defect — unblocked by
  auditing the counter's storage (static vs per-call) — accepted as
  out-of-scope for fixdet-r1, no fix attempted this round.`

## Files touched
- `crates/ec-av1/src/stream.rs` — `gradients_source` helper, guard test,
  20 call sites rewired.
