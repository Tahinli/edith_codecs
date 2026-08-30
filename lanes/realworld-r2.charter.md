# lane-realworld r2 — verify cdef_idx, gate it, then delta_q / delta_lf

Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-realworld`, branch
`lane-realworld`, at bd61617.

## State handed to you (r1's own words; nothing here is verified)
bd61617 is r1's work, committed verbatim by the orchestrator when r1 hit its
turn cap. It compiles (`cargo build -p ec-av1 --release` was clean). Its
**suite status is UNKNOWN** — the test run was launched and the cap hit before
it returned. There is **no gate test at all** yet.

What it contains — CDEF index only, `delta_q`/`delta_lf` entirely unstarted:
- `crates/ec-av1/src/decode.rs`: thread-locals `CDEF_BITS`, `CDEF_TRANSMITTED`,
  `CDEF_SB_COLS`, `CDEF_IDX_GRID`, `CDEF_IDX_HITS` + a `cdef_idx_hits()`
  accessor, mirroring the existing `*_HITS` pattern.
- `maybe_read_cdef_idx(dec, mi_r, mi_c, skip)` — the spec 5.11.56 `read_cdef`
  port. No-op when `cdef.bits == 0` or `skip`; reads once per superblock (this
  decoder's SB is the CDEF unit — no 128x128 support, so no 4-way index).
- Wired at all five sites where `skip` is decoded: `read_intra_mode` (shared by
  `decode_block` and `decode_leaf8`, given new `mi_r`/`mi_c` params),
  `read_intra_mode_rect` / `decode_block_rect`, `decode_inter_block` (~6462),
  `decode_inter_block8` (~8189).
- Both tile functions (`decode_key_frame_tile_with_cdfs`,
  `decode_inter_frame_tile_with_cdfs`) set `CDEF_BITS` / `CDEF_SB_COLS` and init
  `CDEF_IDX_GRID` before the SB loop, and reset `CDEF_TRANSMITTED = false` at
  the top of each SB.
- `apply_cdef` looks up a per-superblock `sidx` through a
  `strength_idx(mi_r, mi_c)` closure over `CDEF_IDX_GRID`, replacing every
  hardcoded `[0]` strength index.
- `crates/ec-av1/src/stream.rs`: the `header.cdef.bits != 0` refusal in
  `decode_stream` was REMOVED.

**Risk r1 flagged:** `apply_cdef`'s early-return guard became
`cdef.bits == 0 && <all index-0 strengths zero>`, so a `bits > 0` stream now
always walks the full filter loop even when every strength is zero. Believed
correct, never checked against a real decode.

## Do these in order — COMMIT AFTER EVERY GREEN STEP, never batch
1. `export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-realworld` then
   `EC_AV1_REQUIRE_AOMENC=1 nice -n 19 cargo test -p ec-av1 --lib -j4`.
   Report the pass count against the 232 passed / 0 failed baseline. If it is
   red, fixing bd61617 is the whole job until it is green.
2. **The CDEF gate.** Copy the structure of
   `a_real_aomenc_stream_with_masked_compound_decodes_pixel_exact`
   (stream.rs around 4447-4657) but keep `--enable-cdef=1` (drop that gate's
   `--enable-cdef=0`) and disable the unrelated features (masked-comp,
   interintra, obmc, warp, restoration, palette, intrabc) exactly as that gate
   does. Hard-assert `crate::decode::cdef_idx_hits() > 0` — the accessor
   already exists. Bound the ffmpeg generate with `-t <seconds>`; build the
   fixture through `gradients_source(seed, w, h, tail)`; aomenc
   `--threads=1 --row-mt=0`. Flat gradients may not make aomenc's RD choose
   `cdef_bits > 0` — if the counter stays 0 across attempts, switch the source
   to `mandelbrot` the way the masked-compound gate does (bounded with `-t`).
   A gate that cannot prove cdef_idx fired is vacuous (class
   `gate-blind-to-feature`), and a firing count is a HARD assert, never a
   warning. COMMIT.
3. **Then** `delta_q` / `delta_lf` (spec 5.11.15 `read_delta_qindex`, 5.11.16
   `read_delta_lflevel`; libaom `av1/decoder/decodemv.c` `read_delta_qindex` /
   `read_delta_lflevel`). This needs new ADAPTING CDFs — four wiring sites:
   struct field, defaults array, save/restore, per-frame counter reset (see
   `cdf_state.rs:590+`; in this codebase reset2/reset3 are length-generic and
   save/restore is a whole-struct Clone, so the defaults array is the one that
   needs hand-checking, but verify the counter reset covers the new tables —
   class `cdf-counter-not-reset`: a table missing from the reset gives right
   values at the wrong adaptation rate). It also needs the running
   quantizer/loop-filter-delta state threaded through block decode, and the
   deltas are read once per superblock at the first non-skip block.
   Its own gate, its own firing counter, its own commit.

## Method
CLASS `compare-range-not-tell`: compare msac RANGE against the oracle, never
`tell()`. Oracle `~/.cache/aom-oracle`; rungs `EC_TRACE=1` (partitions),
`EC_TRACE_COEFF=1`, `EC_TRACE_MODE=1` (inter + intra mode info),
`EC_AV1_PREFILT_DUMP=<prefix>` (pre-filter recon — the right rung for
separating a CDEF filter bug from a symbol bug). Add a rung with
`scripts/instrument-aom-oracle.sh` + `scripts/build-aom-oracle.sh` in the
existing shape (env-gated, silent when unset, idempotent) if you need one.
CLASS `equal-range-means-unread`: reference range unchanged where ours moves =
we read a symbol it never wrote; theirs moves and ours does not = we skipped one.

## Hard rules
- Foreground builds, `nice -n 19 cargo ... -j4`, own `CARGO_TARGET_DIR` as
  above. Sibling worktrees (edith_codecs, -chroma, -lr, -superres, -tiles) have
  live agents — never build in or edit them.
- NEVER push, never merge, never touch `main`. Commit on `lane-realworld` only.
- 75-turn cap. Two agents have now burned a cap on this lane. Do not re-read
  what this charter already told you (class `worker-cap-spent-reading`), and
  commit at every green step.
- Refuse-by-name rather than desync. Never write a refusal string claiming the
  encoder cannot emit a case unless you proved it.
- End with `lanes/realworld-r2.report.md`, VERDICT on the first line: what
  landed, gate names + firing counts, remaining refusal strings verbatim, and
  the next lever.
