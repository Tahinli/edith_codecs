# lane-sbpart r8 — one symbol away

At 376a086 (I merged main for you). Read `lanes/sbpart-r7.report.md`.

r7 range-laddered block2's luma coefficient read against a real aomdec and got
the defect down to a single symbol: **entry range, `eob` and `base_eob` all
match the oracle exactly; the first divergence is the `base` symbol at scan
position (row=1, col=0), pos=32** — we decode 3 (triggering an extra `br` read),
libaom decodes level 1. Same entry state on both sides, different value out.
That shape points at a CDF-row selection, not at neighbour arithmetic.

## Order
1. **First move, named by r7:** check whether `TxbSet::Luma64`'s CDF-set /
   `txs_ctx` resolution matches libaom's `get_txsize_entropy_ctx(TX_32X64)`
   (`cdf_state.rs`, search `Luma64`). A wrong CDF-set produces exactly this
   symptom. Related class: a table narrowed to a pinned row breaks the moment
   the indexing field moves ([[cdf-row-held-constant]]).
2. Only if that matches: `base_ctx`'s neighbour math at (row=1, col=0) for a
   true `TX_32X64`. Remember that a scan weight or step can use the CROSS axis
   and that square candidates hide every axis swap
   ([[scan-weights-cross-axis]]) — if you touch it, sweep the transposed copy in
   the same round.
3. Fix, get the gate pixel-exact, commit. Then the round is done; do not start
   stage 2 (the 32x32 `part32` values) or the inter path.

Ruled out and not to be revisited: `around_rect` (skip_ctx and dc_sign_ctx both
read identically to the oracle), the CDEF per-SB guard, and — from r5 — the
theory that this is a reconstruction bug at all.

Reproduction, from r7: the pinned OBU is scratchpad-only and does not survive;
regenerate with
`EC_SBPART_GATE_ATTEMPTS=1 EC_AV1_GATE_DUMP=<path> EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib a_real_aomenc_stream_with_a_superblock_level_horz_vert_partition_decodes_pixel_exact -- --nocapture`
(seed 42 reproduces first try). Oracle rung 11 is live in `~/.cache/aom-oracle`
and captured in `scripts/instrument-aom-oracle.sh`; drive it with
`EC_TRACE_COEFF=1 aomdec --i420 -o /dev/null <pin>.obu`.

## Budget discipline
75 turns, no reset on resume. At about turn 55, stop starting new work: commit
what is green and write your report. The merge is already done for you.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbpart`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms;
`EC_AV1_REQUIRE_AOMENC=1`; aomenc `--threads=1 --row-mt=0 --sb-size=64` and this
gate needs `--enable-tx-size-search=0`. The oracle is SHARED — env-gated rungs
only. Sibling worktrees have live agents — never build in or edit them. Never
push, never merge into main. End with `lanes/sbpart-r8.report.md`, VERDICT on
line 1.
