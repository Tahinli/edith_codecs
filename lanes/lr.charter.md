# lane-lr — loop restoration decode (read_lr + Wiener + self-guided)

Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-lr`, branch `lane-lr`, off main 87a1e34.

## Goal
Remove the whole-frame refusal at `crates/ec-av1/src/stream.rs:195`
("a frame with loop restoration enabled") by actually decoding it:
1. the per-restoration-unit symbols read from the tile data during the
   superblock walk (spec 5.11.57 `read_lr` / `read_lr_unit`), and
2. the two restoration filters themselves (spec 7.17: Wiener 7.17.4,
   self-guided 7.17.2/7.17.3), applied to the CDEF-filtered frame.

## Anchors
- Spec: 5.11.57 `read_lr`, 5.11.58 `read_lr_unit`, 6.10.15, 7.17 `loop restoration process`.
- libaom (oracle source, `~/.cache/aom-oracle` build tree — find the src dir under it):
  `av1/decoder/decodeframe.c` `read_lr()` / `read_lr_unit()`;
  `av1/common/restoration.c` `av1_loop_restoration_filter_frame`,
  `wiener_filter_stripe`, `selfguided_restoration`, `av1_selfguided_restoration_c`,
  and the `sgr_params` table.
- Existing syntax: `crates/ec-av1-syntax/src/frame.rs:341` `LoopRestorationParams`
  (`frame_restoration_type`, `loop_restoration_size`, `uses_lr`).
- The superblock walk in the decoder is `crates/ec-av1/src/decode.rs` around
  lines 4547 and 9275 (`for sb_r in 0..sb_rows { for sb_c ... }`). `read_lr` is
  called once per superblock, BEFORE `decode_partition`, and loops over the LR
  units whose top-left falls in this superblock, per plane.
- CDF tables live in `crates/ec-av1/src/cdf.rs`. There is currently NO
  restoration CDF (`grep -i restoration crates/ec-av1/src/cdf.rs` is empty) —
  you must add `restore_wiener`, `restore_sgrproj`, `restore_switchable`
  (the switchable one is a 3-symbol CDF) with libaom's `default_*_cdf` values
  copied EXACTLY from `av1/common/token_cdfs.h` / `entropymode.c`. Note that
  the Wiener/SGR *coefficients* are NOT CDF-coded: they use
  `decode_signed_subexp_with_ref` (literal + subexp bits), spec 5.9.27-ish.
- CDF WIRING IS FOUR SITES in this codebase but in practice only the defaults
  array needs hand-checking (reset2/reset3 are length-generic, save/restore is
  a whole-struct Clone). Still verify the counter reset covers new tables.

## Method (this is a range-ladder lane)
CLASS `compare-range-not-tell`: never compare `tell()` across decoders —
compare the msac RANGE after each element. The instrumented oracle at
`~/.cache/aom-oracle` has env-gated rungs: `EC_TRACE=1` (EC_PART), 
`EC_TRACE_COEFF=1` (EC_COEFF), `EC_TRACE_MODE=1` (EC_MODE/EC_IMODE).
If you need an LR rung, ADD one to `scripts/instrument-aom-oracle.sh`
following the exact shape of the existing rungs (env-gated, silent when unset,
idempotent, wrapper-around-impl) and rebuild the oracle with
`scripts/build-aom-oracle.sh`. That is an expected part of this lane.
CLASS `equal-range-means-unread`: if the reference's range is UNCHANGED where
ours moves, we read a symbol it never wrote; if theirs moves and ours doesn't,
we skipped one. Only when both move by different amounts is a table implicated.

## Staging — commit after EVERY green milestone
1. **Syntax only.** Read the lr symbols correctly and then still refuse the
   frame by name ("loop restoration symbols are read but the filters are not
   applied"). Prove correctness by the partition walk surviving: a stream that
   previously desynced into out-of-alphabet garbage now decodes its partitions
   and only fails the pixel compare. COMMIT.
2. **Wiener.** Implement `wiener_filter_stripe` (64-row stripes with the
   3-pixel stripe boundary handling — that boundary handling is the classic
   trap; libaom saves/restores `rlbs` boundary lines). Gate a stream forced to
   Wiener only. COMMIT.
3. **Self-guided.** `selfguided_restoration` incl. the box-sum radii and the
   `sgr_params` table. COMMIT.
4. **Switchable.** Remove the last refusal. COMMIT.

## Gate (mandatory, in `crates/ec-av1/src/stream.rs` next to the existing gates)
- Follow the existing gate shape exactly. `EC_AV1_REQUIRE_AOMENC=1` must be set
  when you run tests, so a missing oracle FAILS rather than SKIPs.
- Bound every ffmpeg `generate` with `-t <seconds>` (an unbounded source
  deadlocked a gate for an hour; class `gate-loader-slurps-whole-file`).
- Build the fixture through the existing `gradients_source(seed, w, h, tail)`
  helper — do NOT hand-write a `gradients=size=` string; ffmpeg's gradients
  source ignores its seed and the helper derives colours from the seed instead.
- aomenc flags: `--threads=1 --row-mt=0`, and whatever turns LR on
  (it is on by default; `--enable-restoration=1`). To force a single filter
  type, encode content that provokes it, or use the aomenc knob if one exists —
  if not, assert per-type firing counts instead.
- The gate MUST hard-assert a firing count (e.g. `wiener_hits > 0`), a
  thread-local `Cell<usize>` counter in decode.rs like the existing `*_HITS`
  (they are thread-local now, NOT atomics — process-global atomics made one
  gate count another test's work). CLASS `gate-blind-to-feature`: a gate that
  cannot prove its feature fired is vacuous.
- Refusals inside the gate are FORBIDDEN once the stage that removes them
  lands: assert the decode returns Ok, do not tolerate an Err.

## Hard rules
- Own build dir: `export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-lr`.
  Foreground builds only, `nice -n 19 cargo ... -j4`. Never `cargo` in another
  worktree, never touch a sibling worktree's files.
- Suite scope: `nice -n 19 cargo test -p ec-av1 --lib` (67 s baseline on main,
  232 passed / 0 failed). It must stay green.
- NEVER push, never merge, never touch `main`. Commit on `lane-lr` only.
- You have a 75-turn cap. **Commit work-in-progress after every green
  milestone** — five builders have lost complete implementations at the cap.
  If you are near the cap, commit whatever compiles as `wip(av1): ...` and
  write `lanes/lr.report.md` before you stop.
- Refuse-by-name rather than desync: any case you do not implement must return
  `Error::unsupported` with an accurate string. CLASS
  `refusal-claim-disproved-by-its-own-gate`: never write a refusal string that
  claims the encoder never emits the case unless you have proved it.
- Write `lanes/lr.report.md` at the end: what landed, gate name + firing count,
  what still refuses and by what exact string, and the next lever.
