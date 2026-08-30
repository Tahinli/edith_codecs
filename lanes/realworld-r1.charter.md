# lane-realworld r1 charter — the first two blockers on a DEFAULT stream

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-realworld, branch
lane-realworld @ main.
Build/test ONLY:
  `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-realworld CARGO_BUILD_JOBS=4 nice -n 19 cargo test -p ec-av1 --release --lib <name> -- --nocapture`
FOREGROUND, EC_AV1_REQUIRE_AOMENC=1 on gates. Never push. Never touch the main
checkout or the sibling -chroma worktree. WIP COMMIT after every green step.

## The measurement this lane exists for
Every gate in this crate encodes with a long list of `--enable-*=0` flags. A
stream encoded with DEFAULT settings — nothing disabled — refuses on the very
first frame. Walking the chain by disabling one feature at a time gives the
real-world blocker order:

  1. `cdef_bits > 0`            <- a default stream stops here
  2. `delta_q_present || delta_lf_present`
  3. a block that actually uses a palette
  4. a partition below 8x8
  5. a smooth or paeth chroma mode        (lane-chroma has this)
  6. a partition below 16x16 other than a clean split

This lane takes 1 and 2, because nothing else matters until a default stream
gets past them.

## 1. CDEF index (the bigger win, do it first)
The CDEF FILTER is already implemented and working — `cdef` appears ~108 times
in this crate and streams with `cdef_bits == 0` decode pixel-exact today. What
is missing is only the per-64x64 `cdef_idx` SYMBOL: when `cdef_bits > 0` the
frame header carries 2^bits strength pairs and each 64x64 block reads an index
selecting which pair applies.

Read `read_cdef` in `~/.cache/aom-oracle/src/av1/decoder/decodeframe.c` and the
spec's `read_cdef` (5.11.56). Points to get right:
- the symbol is a LITERAL of `cdef_bits` bits, not an adapting CDF;
- it is read once per 64x64 in the superblock, skipped when the block is
  entirely skip, and NOT read at all when `coded_lossless` or `allow_intrabc`;
- the index selects into the header's already-parsed strength arrays, which
  this crate's syntax layer reads — check before re-parsing anything.

## 2. delta_q / delta_lf
Per-superblock `delta_q_abs` / `delta_lf_abs` symbols with their own CDFs, plus
carrying the running quantizer (and per-plane loop-filter deltas) through the
block decode. Anchor: spec 5.11.15 `read_delta_qindex` / 5.11.16
`read_delta_lflevel`, and libaom's `read_delta_q`/`read_delta_lf` in decodemv.c.
Any new adapting CDF must be wired at ALL FOUR sites — struct field, defaults,
save/restore, per-frame counter reset.

## Method and gates
Refuse-by-name first, decode second: a desync is never acceptable, a precise
refusal always is. On a pixel mismatch, self-pin with `EC_AV1_GATE_DUMP` and
compare the msac RANGE (never `tell()`) against the instrumented oracle — the
rungs are `EC_TRACE=1` (partitions), `EC_TRACE_MODE=1` (inter AND intra mode
info), `EC_TRACE_COEFF=1` (coefficients). Class rule worth remembering: if the
oracle's range is UNCHANGED where ours moves, we are reading a symbol it never
wrote — check that before suspecting a table.

Add a GATE for each feature, with a hard-asserted firing count, following the
shape of the existing gates. A gate that cannot fire the feature it names
measures nothing; this batch has already fixed three of those.

## Done criteria
Full lib suite stays green (232 passed / 0 failed on main today). A stream
encoded with `--enable-cdef=1` (the default) and one with delta-q enabled each
decode pixel-exact against ffmpeg, proven by a hard-asserted gate. REPORT
`lanes/realworld-r1.report.md`, VERDICT on the FIRST line, and re-walk the
blocker chain at the end so the report says exactly which blocker a default
stream now stops at.
