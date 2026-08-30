# lane-lr r2 — read_lr wiring, then the two filters

Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-lr`, branch `lane-lr`,
at 4d7ae64.

## Read these two files first, then start writing code
- `lanes/lr.report.md` — r1's complete design writeup. Its "Next lever" section
  has every formula and `file:line` you need. Do NOT re-derive what it already
  contains (class `worker-cap-spent-reading`).
- `lanes/lr.charter.md` — the original charter: staging, gate rules, hard rules.
  Still binding.

## What r1 landed (both committed, suite confirmed green 232/0 on this tree)
- `crates/ec-av1/src/cdf.rs`: `RESTORE_WIENER = [11570, 32768, 0]`,
  `RESTORE_SGRPROJ = [16855, 32768, 0]`,
  `RESTORE_SWITCHABLE = [9413, 22581, 32768, 0]` — copied from libaom
  `entropymode.c` `default_{wiener,sgrproj,switchable}_restore_cdf` and
  converted to this codebase's `[a0, (a1,) 32768, 0]` convention, checked
  against `INTRABC`'s own `AOM_CDF2(30531) -> [30531, 32768, 0]`.
- `crates/ec-av1/src/cdf_state.rs`: the three tables wired into `Cdfs` —
  fields, `new()` defaults, `reset_counts` via `reset1` — following `intrabc`.
- Nothing reads them from the bitstream yet; the whole-frame refusal at
  `crates/ec-av1/src/stream.rs:195-198` is unchanged.

## r1's decisive facts, so you do not re-find them
- `restoration.c:1277` `av1_loop_restoration_corners_in_sb` returns 0 unless
  `bsize == sb_size`, so "read LR once per superblock" is spec-exact here, not
  a simplification — every restoration unit is >= 64px.
- `msac.rs:348` `SymbolDecoder::literal(bits)` is the `L(n)` primitive LR's
  subexp reads need. The existing `decode_subexp` /
  `decode_unsigned_subexp_with_ref` / `decode_signed_subexp_with_ref` /
  `inverse_recenter` at `ec-av1-syntax/src/frame.rs:1797-1848` are
  `BitReader`-only (uncompressed frame header) — LR needs a SECOND,
  msac-flavoured port, not reuse.
- Threading point: `decode_key_frame_tile_with_cdfs` (decode.rs:4478) and
  `decode_inter_frame_tile_with_cdfs` (decode.rs:9088) already take long
  positional argument lists including cdef and loop_filter. Add
  `&LoopRestorationParams` there and pass `&LoopRestorationParams::default()`
  from the public wrappers (`decode_key_frame_tile:4435`,
  `decode_inter_frame_tile:9030`) — that keeps ~10 test call sites untouched.
- Wiener tap ranges `(-5, 10, k1)` `(-23, 8, k2)` `(-17, 46, k3)`;
  SGR ranges `(-96, 31, k4)` `(-32, 95, k4)`. `av1_sgr_params` is in libaom
  `restoration.c`.

## Order of work — COMMIT AFTER EVERY GREEN MILESTONE
1. Port the msac-flavoured subexp/recenter helpers with their own unit tests
   pinned against known values. COMMIT.
2. `read_lr` / `read_lr_unit` called once per superblock before
   `decode_partition` at decode.rs ~4547 (key frame) and ~9275 (inter), with
   the per-plane unit-grid formula from the report. The frame still refuses,
   by a NEW and accurate name ("loop restoration symbols are read but the
   filters are not applied"). The proof this stage works: a stream that used to
   desync the partition walk into out-of-alphabet garbage now walks its
   partitions cleanly and fails only the pixel compare. COMMIT.
3. Wiener filter, incl. the 3-pixel stripe boundary save/restore (libaom's
   `rlbs`) — that boundary handling is the classic trap. COMMIT.
4. Self-guided, incl. the box-sum radii and `av1_sgr_params`. COMMIT.
5. Switchable; remove the last refusal; the gate asserts Ok, never an Err.
   COMMIT.

## Gate rules (mandatory, unchanged from r1's charter)
`EC_AV1_REQUIRE_AOMENC=1` on every test run; `-t <seconds>` on every ffmpeg
generate; fixtures through the existing `gradients_source(seed, w, h, tail)`
helper (ffmpeg's `gradients` ignores its own seed); aomenc
`--threads=1 --row-mt=0 --enable-restoration=1`; a HARD-asserted firing count
via a thread-local `Cell<usize>` counter matching the existing `*_HITS`
(thread-local, NOT atomics). A gate that cannot prove LR fired is vacuous.

Note: main now carries a `gate_coverage` guard that derives, from the gate
source, which aomenc tools are switched off in all 20 gates and on in none.
`enable-restoration` is one gate short of that list today; your gate enabling it
is exactly the right outcome.

## Hard rules
- `export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-lr`. Foreground builds,
  `nice -n 19 cargo ... -j4`. The suite takes ~4 minutes on this tree — give
  any `cargo test` call a timeout of at least 600000 ms; do not let a 120 s
  default kill it and then background it unpolled, as r1 did.
- Sibling worktrees (edith_codecs, -chroma, -realworld, -superres, -tiles) have
  live agents. Never build in or edit them.
- Baseline to hold: 232 passed / 0 failed on this tree (main is at 234 with two
  new guard tests you will inherit at merge).
- NEVER push, never merge, never touch `main`. Commit on `lane-lr` only.
- 75-turn cap: commit at every green step; near the cap, commit whatever
  compiles as `wip(av1): ...` and update `lanes/lr.report.md`.
