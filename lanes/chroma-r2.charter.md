# lane-chroma r2 — smooth-neighbour edge filter, then smooth/paeth chroma

Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-chroma`, branch
`lane-chroma`, at 3ca2455 (r1's report; r1 landed NO code — it reverted).

## What r1 established (read `lanes/chroma-r1.report.md` first — it is short)
- The `SMOOTH_PRED`/`PAETH_PRED` predictor math in `intra.rs` is already
  spec-correct and shared between luma and chroma. Removing the chroma refusal
  (`decode.rs` uv_mode 9..=12, around lines 1970 and 2458) is NOT the work.
- The real defect is on LUMA: `reconstruct()` hardcodes `smooth_neighbor = false`
  — its own comment calls this "a corner-cut for luma" and names the upgrade
  path. `intra_edge_filter_strength` therefore picks the wrong bucket whenever a
  directional block's neighbour used a SMOOTH mode. r1 reproduced it exactly:
  seed 42, directional block px=48 py=16 mode=5, neighbour px=48 py=0 with
  SMOOTH_H_PRED, off by 1-2 in the filtered edge.
- **Nothing in this repo had ever exercised it**: all 23 real-aomenc gates in
  `stream.rs` pass `--enable-smooth-intra=0 --enable-paeth-intra=0`. That is a
  fleet-wide gate-recipe blindness, not a one-gate miss.
- r1's attempted diff (refusal removal + the new gate + the counter) is
  preserved at `lanes/chroma-r1-attempt.diff` in this worktree. Start from it.

## Goal, in order
1. **Fix the luma bug first, on its own, with its own gate.** Thread the
   neighbour's intra mode into `reconstruct()` / `reconstruct_rect()` so
   `smooth_neighbor` is `above_mode` (or `left_mode`, per the spec's
   directionality) being in `SMOOTH_PRED..=SMOOTH_H_PRED`. r1's report lists
   the ~9 call sites. `Neighbours` already tracks per-block state — add the
   luma mode alongside whatever it already stores; follow the shape of the
   existing `skip_at` / `skip_txfm_ctx` accessors in `decode.rs`.
   Spec anchor: 7.11.2.9 `intra edge filter strength` — the `filt_type`
   argument is 1 when the relevant neighbour used a smooth mode. libaom:
   `av1/common/reconintra.c` `get_filt_type()` / `av1_filter_intra_edge` and
   `intra_edge_filter_strength`; `get_filt_type` reads
   `av1_is_directional_mode` and `is_smooth(above_mbmi)`/`is_smooth(left_mbmi)`.
   Gate it with a real-aomenc stream that enables smooth intra for LUMA while
   the chroma refusal is still in place (chroma can still refuse at this
   stage — that keeps the two changes separable). COMMIT.
2. **Then** add chroma UV-mode neighbour tracking and remove the smooth/paeth
   chroma refusal, with the gate from the attempt diff
   (`a_real_aomenc_stream_with_smooth_paeth_chroma_decodes_pixel_exact`)
   passing pixel-exact on luma AND chroma. COMMIT.
3. **Then** the directional-chroma sibling if turns remain: `decode.rs:7786`,
   the inter-frame intra-block reader, which refuses any non-DC/CFL uv_mode
   unconditionally and never computes `angle_delta_uv`. r1 found no bug there,
   only unwritten plumbing. COMMIT.

## Gate rules (mandatory)
- Run tests with `EC_AV1_REQUIRE_AOMENC=1` — a missing oracle must FAIL, not SKIP.
- Bound every ffmpeg `generate` with `-t <seconds>`.
- Build fixtures through the existing `gradients_source(seed, w, h, tail)`
  helper; ffmpeg's `gradients` source ignores its own seed.
- aomenc `--threads=1 --row-mt=0`, and of course `--enable-smooth-intra=1`
  (and `--enable-paeth-intra=1` for stage 2).
- Hard-assert a firing count with a thread-local `Cell<usize>` counter in
  decode.rs matching the existing `*_HITS` (thread-local now, NOT atomics):
  for stage 1, count the blocks where `smooth_neighbor` came out TRUE — a gate
  that runs with smooth intra enabled but never hits the new branch proves
  nothing (class `gate-blind-to-feature`).
- Refusals inside a gate are FORBIDDEN once the stage removing them lands.

## Method
CLASS `compare-range-not-tell`: compare the msac RANGE against the oracle, never
`tell()`. Oracle at `~/.cache/aom-oracle`, rungs `EC_TRACE=1` (partitions),
`EC_TRACE_COEFF=1`, `EC_TRACE_MODE=1` (inter + intra mode info),
`EC_AV1_PREFILT_DUMP=<prefix>` (per-frame pre-filter recon — the right rung for
a predictor bug, since it isolates prediction from the loop filters).
Add a rung via `scripts/instrument-aom-oracle.sh` + `scripts/build-aom-oracle.sh`
in the existing shape (env-gated, silent when unset, idempotent) if you need one.

## Hard rules
- `export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-chroma`. Foreground builds,
  `nice -n 19 cargo ... -j4`. Sibling worktrees (edith_codecs, -realworld, -lr,
  -superres, -tiles) have live agents — never build in or edit them.
- Suite: `nice -n 19 cargo test -p ec-av1 --lib`, baseline 232 passed / 0 failed.
- NEVER push, never merge, never touch `main`. Commit on `lane-chroma` only.
- 75-turn cap: COMMIT AFTER EVERY GREEN MILESTONE. r1 spent its whole budget and
  landed zero code because it held everything in the working tree; a commit that
  compiles beats a perfect uncommitted tree. Do not re-derive what r1's report
  already tells you (class `worker-cap-spent-reading`).
- End with `lanes/chroma-r2.report.md`, VERDICT on the first line.
