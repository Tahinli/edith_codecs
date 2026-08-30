# lane-chroma r3 — run the smooth-intra gate, then the chroma refusal

Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-chroma`, branch
`lane-chroma`.

## State
- `8bca1d6` — **the luma fix landed**: `smooth_neighbor` is threaded into the
  intra edge filter strength instead of being hardcoded `false`. This is r1's
  root cause (a directional block neighbouring a smooth-mode one picked the
  wrong filter bucket). Committed, compiles.
- The next commit is the orchestrator's cap-rescue of r2's gate: ~140 new lines
  in `crates/ec-av1/src/stream.rs`, a smooth-intra gate that **compiles but has
  never been run once**. That is your first job.
- `lanes/chroma-r2.charter.md` — still binding for everything below.
- `lanes/chroma-r1.report.md` + `lanes/chroma-r1-attempt.diff` — r1's root-cause
  writeup and its reverted attempt (the chroma-side gate lives there).

## Order — COMMIT AFTER EVERY GREEN STEP
1. Run the new gate:
   `export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-chroma` then
   `EC_AV1_REQUIRE_AOMENC=1 nice -n 19 cargo test -p ec-av1 --lib <gate name> -j4 -- --nocapture`
   with a timeout of at least 600000 ms. Make it pass pixel-exact on luma with a
   HARD-asserted firing count proving `smooth_neighbor` actually came out TRUE
   somewhere — a gate that enables smooth intra but never hits the new branch
   proves nothing (class `gate-blind-to-feature`). Then the full suite
   (baseline 232/0 on this tree; main is at 234 with two new guard tests).
   COMMIT.
2. Chroma: add UV-mode neighbour tracking to `Neighbours` and remove the
   smooth/paeth chroma refusals (decode.rs uv_mode 9..=12, around 1970 and
   2458), with r1's gate
   `a_real_aomenc_stream_with_smooth_paeth_chroma_decodes_pixel_exact` from the
   attempt diff green on luma AND chroma. COMMIT.
3. Once a gate really enables smooth-intra and paeth-intra, main's
   `crates/ec-av1/src/gate_coverage.rs` guard will FAIL by design: its
   `NEVER_EXERCISED` list still names `enable-smooth-intra` and
   `enable-paeth-intra` as tools no gate exercises. Deleting those two entries
   is part of landing this work — that guard exists precisely so closing the
   hole is noticed. (The guard is on main, so you will meet it at merge if not
   before; if `gate_coverage.rs` is not in your tree yet, note it in the report
   for the merge instead.)
4. If turns remain: the directional-chroma sibling at `decode.rs:7786`, the
   inter-frame intra-block reader, which refuses any non-DC/CFL uv_mode
   unconditionally and never computes `angle_delta_uv`. No known bug there,
   just unwritten plumbing. COMMIT.

## Gate rules (mandatory)
`EC_AV1_REQUIRE_AOMENC=1` on every run; `-t <seconds>` on every ffmpeg generate;
fixtures through `gradients_source(seed, w, h, tail)`; aomenc
`--threads=1 --row-mt=0 --enable-smooth-intra=1` (and `--enable-paeth-intra=1`
for stage 2); firing counts are HARD asserts via thread-local `Cell<usize>`
counters like the existing `*_HITS`.

## Hard rules
- Own `CARGO_TARGET_DIR` as above; foreground builds, `nice -n 19 cargo ... -j4`;
  every `cargo test` gets a timeout of at least 600000 ms.
- Sibling worktrees (edith_codecs, -realworld, -lr, -superres, -tiles) have live
  agents. Never build in or edit them.
- NEVER push, never merge, never touch `main`. Commit on `lane-chroma` only.
- 75-turn cap and it does NOT reset on resume: commit at every green step.
- End with `lanes/chroma-r3.report.md`, VERDICT on the first line.
