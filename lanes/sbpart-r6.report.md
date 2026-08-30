VERDICT: PARTIAL -- merged main and compiled clean (charter's order 1), built
oracle rung 10 and range-laddered block2's mode-info reads (charter's order
2-3), got a decisive result that OVERTURNS r5's own framing of where the
desync starts, but did not reach a fix (order 4) before budget ran thin.

## 1. Merge main (170a5a3), compiled

`git merge origin/main` conflicted in exactly one file, `stream.rs`: both
lane-realworld (deltaq gate) and lane-tiles (tile-group + non-uniform-spacing
gates) added tests immediately adjacent to this lane's sbpart tests with no
shared context lines between them, so git's 3-way merge produced one large
interleaved conflict block spanning ~630 lines rather than several small
ones. First attempted resolution (character-level "keep both sides") was
WRONG -- it interleaved unrelated functions' local variables mid-body.
Discarded that with `git checkout --merge` and re-resolved by function
boundary instead: every complete test function from each side kept intact,
in sequence (sbpart's rect64 gate + pinned replay, then realworld's deltaq
gate, then tiles' r7/r9/non-uniform-spacing gates) -- no duplicated or
truncated function bodies. `cargo check -p ec-av1 --tests`: clean, only
pre-existing missing-doc warnings. Committed at bfb1b8f.

## 2. Oracle rung 10

Rung 9 (charter's assignment) was already taken by lane-lr on `main` by the
time this round started (`calculate_intermediate_result` export, landed on
main before this merge). Took rung 10 instead, in
`scripts/instrument-aom-oracle.sh`: `EC_TRACE_MODE_STEP=1` prints
`EC_ISTEP mi_row=.. mi_col=.. name=.. val=.. rng=..` after each of `skip`,
`cdef`, `dq`, `mode`, `angle_y`, `uv_mode`, `angle_uv` inside
`ec_read_intra_frame_mode_info_impl` (rung 5's renamed body) -- the finest
granularity the charter asked for, one line per symbol, not one line per
block. Built clean after fixing a Python `%`-operator escaping bug (raw `%d`/
`%u` C format specifiers in the template collided with Python's own `%`
substitution on the first attempt -- caught by the ninja build failing with
"missing terminating \" character", reverted the bad decodemv.c edit with a
targeted regex strip before rebuilding, not a full `git checkout` of the
oracle source since rungs 1-9's uncommitted patches live in the same
uncommitted working tree). `ninja -C ~/.cache/aom-oracle/build aomdec`:
clean after the fix.

Added the matching instrumentation to our own decoder
(`read_intra_mode_rect` in `decode.rs`, gated on the same `EC_TRACE_MODE_STEP`
env var, same `EC_ISTEP` line format) so the two traces diff directly by
`grep`, no manual field-mapping between differently-shaped trace formats.

## 3. Range-ladder result: block2's mode-info prefix is byte-exact

Regenerated the pinned mismatch fresh this round (old pin was gone --
scratchpad reaped between sessions per [[oracle-in-reaped-dir]]'s pattern,
though this time it was just session-scoped, not a tmpfs wipe):
`EC_SBPART_GATE_ATTEMPTS=1 EC_AV1_GATE_DUMP=... cargo test ... a_real_aomenc_
stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact --
--nocapture`, seed 42, same recipe as r2-r5. Decoded the pinned OBU through
both `aomdec --i420` (oracle) and `pinned_sbpart_stream_decodes_pixel_exact`
(ours), both with `EC_TRACE_MODE_STEP=1`.

Block2 is `mi_row=0 mi_col=24` (px 96, matching r5's bounding-box finding).
Side-by-side range ladder, every field:

```
              ours                          oracle
skip     val=0 rng=43948               val=0 rng=43948
cdef     val=0 rng=43948               val=0 rng=43948
dq       rng=43948                     val=180 rng=43948
mode     val=0 rng=44880               val=0 rng=44880
angle_y  val=0 rng=44880               val=0 rng=44880
uv_mode  val=0 rng=63764               val=0 rng=63764
angle_uv val=0 rng=63764               val=0 rng=63764
```

Every decoded value and every `rng` after every read matches exactly. This
is a real per-symbol proof, not inspection -- it **overturns r5's own
framing** ("block2 ... wrong from its very first pixel" was read as "desync
at block2's first symbol"; it is not). The mode-info prefix
(`read_intra_mode_rect`, `decode.rs:2572`) is entropy-correct for block2.
The desync -- confirmed still real by the full oracle trace only ever
visiting `mi_col ∈ {0, 16, 24, 32}` at row 0 and `{0, 16, 32}` at row 16
(three `NONE` 64x64 blocks plus the one `VERT` pair, seven blocks total),
while our own trace's next `EC_ISTEP` entry after block2 is
`mi_row=16 mi_col=40` -- a rect64 block **that does not exist in the real
partition tree at all** (oracle's `mi_col=32` row-1 block is a single
directional `NONE` block, mode=5, never split) -- must originate somewhere
between `angle_uv` returning and the end of `decode_block_rect64`'s own
symbol reads, i.e. **strictly after** the mode-info prefix this round
laddered clean.

## Narrowed suspect for next round

`decode_block_rect64` (decode.rs:3081), after the now-proven-correct prefix,
reads in order: `tx_select`-gated depth symbol (refuses if split; gate's
`--enable-tx-size-search=0` should force `tx_select=false` so this is a
no-op here, unverified this round but low suspicion given r2's precedent fix
comment at decode.rs:2781-2787), then three coefficient reads --
`read_coeffs` with `TxbSet::Luma64`/`scan32` for the 32x32 luma corner, then
`read_coeffs_rect` with `TxbSet::ChromaRect32x16` for U then V -- each keyed
off `Neighbours::around_rect(at, bw, bh)` (decode.rs:2094) for
`skip_ctx`/`dc_sign_ctx`. **`around_rect` is the sharpest unaudited
suspect**: r5's inspection pass covered `record_rect` (the WRITE side after
block1 decodes) and `fill_skip_grid_rect`/`skip_txfm_ctx` (a different
context path), but never looked at `around_rect` (the READ side block2
itself calls, gathering over block1's freshly-written span) -- and per
[[context-read-from-one-cell]], a gather that reads the wrong span or wrong
cell for a just-written neighbour is exactly the class of bug that produces
a wrong CDF selection, which changes both the decoded coefficient VALUES
and the number of bits consumed, matching everything observed (wrong pixels
in block2 from coefficient reconstruction AND a real downstream desync).
Next round should range-ladder the coefficient reads themselves (rung 3,
`EC_TRACE_COEFF`, already built by a sibling lane and reusable as-is) at
block2's luma/chroma planes, comparing against `around_rect`'s computed
`skip_ctx`/`dc_sign_ctx` by hand first -- cheaper than a full range-ladder
since there are only 3 reads to check, and the mode-info ladder built this
round proves the harness/methodology works end to end.

## Hard rules followed

Worked only in this worktree; `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbpart`
every build; foreground `nice -n 19 cargo ... -j4`; `EC_AV1_REQUIRE_AOMENC=1`
on the aomenc-driven run; aomenc recipe unchanged (`--threads=1 --row-mt=0
--sb-size=64 --enable-tx-size-search=0`, inherited from the existing gate,
not re-typed); oracle rung is env-gated (`EC_TRACE_MODE_STEP`), silent when
unset, added to `scripts/instrument-aom-oracle.sh` not left as a standalone
patch; no other worktree touched; no push, no merge into main.

## Next round, in order

1. Range-ladder the three coefficient reads in `decode_block_rect64` for
   block2 (luma corner via `TxbSet::Luma64`, then U/V via
   `TxbSet::ChromaRect32x16`) against oracle rung 3 (`EC_TRACE_COEFF`,
   already built, no new rung needed).
2. Hand-check `around_rect` (decode.rs:2094) against libaom's real
   neighbour-gather for skip/dc-sign context on a rect strip's right-hand
   block -- compare its span math to `record_rect`'s write span for block1,
   which r5 already confirmed correct.
3. Fix, then re-run the full `n_attempts=40` gate and confirm
   `sb_rect_hits() > 0` stays a hard pass, not just a compile check.
