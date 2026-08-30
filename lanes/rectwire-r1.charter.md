# lane-rectwire r1 charter — wire the rectangular transform into decode.rs

## Where you are
Worktree /home/tahinli/Documents/Code/Rust/edith_codecs-rectwire, branch
lane-rectwire @ main (6e54e47).
Build/test ONLY:
  `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-rectwire CARGO_BUILD_JOBS=4 nice -n 19 cargo test -p ec-av1 --release --lib <name> -- --nocapture`
FOREGROUND, EC_AV1_REQUIRE_AOMENC=1 on gates. Never push. Never touch the main
checkout or the sibling -gm worktree. WIP COMMIT after every green milestone.

## The primitive already exists — do not rebuild it
lane-recttx landed the rectangular inverse transform this lane consumes:
`transform.rs::inverse_transform_2d_typed_wh(dequant, w, h, bit_depth, tx_type)`
and `dequant_and_inverse_typed_wh`, with `quant.rs::dequant_wh` beside them.
All 14 rect sizes are pinned by checksum against real libaom kernels
(`transform.rs::tests::rect_sizes_pinned_against_libaom`) and the square path
is proven bit-identical. Read `lanes/recttx-r1.report.md` first. The transform
MATH is done; this lane is the decode-side wiring it named as still owed.

## What is missing (the report's own list)
1. **Rectangular scan order.** Only square zig-zag/diagonal tables are wired.
   libaom's scan tables live in `~/.cache/aom-oracle/src/av1/common/scan.c`
   (`av1_default_scan_<w>x<h>` and the class/`av1_scan_orders` table).
2. **Rectangular eob context.** The `get_eob_ctx` family is keyed on a square
   tx size class today; libaom keys it on the real tx size
   (`av1/common/txb_common.h`, `av1/decoder/decodetxb.c`).
3. **`max_txsize_rect_lookup` threading** through partition and tx-size
   selection, so a rect block asks for its real tx size instead of a square
   approximation.

## Two refusals this is meant to lift, in priority order
- `decode.rs`'s refusal of every INTER partition below 16x16 (grep
  "an inter partition below 16x16"). This is why the 8x8-OBMC gate has never
  fired one 8x8 OBMC block in 80 attempts, despite the encoder writing them
  constantly.
- The intra HORZ/VERT strips' skip-only restriction (grep "a non-skip HORZ/VERT
  intra strip"), and the strip refusal in screen-content frames beside it.

Lifting EITHER one with pixel-exact evidence is a good round. Do not attempt
both if the first is not clean — a partial landing that refuses precisely is
worth more than a wide one that decodes wrong.

## Traps this repo has already paid for — read these before you start
- **Cross-axis scan weights.** A scan weight or step can use the CROSS axis
  where libaom says so; square candidates hide every axis swap. Sweep a WxH
  case together with its HxW twin in the SAME test, always.
- **A reference's layout is not the spec's.** libaom's coefficient buffer is
  column-major; ours is row-major. Transpose at the boundary, never in our
  code, or a real axis bug gets cancelled by a transposed harness.
- **A new tx size threads more surfaces than you expect** — scan, eob, context
  derivation, the tx-set tables. Grep the reference for every indexed surface
  and sweep them in ONE round rather than discovering them one desync at a time.
- **Adapting CDF four-site wiring**: struct field, defaults, save/restore,
  per-frame counter reset. A table missing from the counter reset has the right
  values and the wrong adaptation rate, and desyncs only after many reads.

## Method
Refuse-by-name first, decode second: it is always acceptable to read every
symbol correctly and refuse the reconstruction by name. A desync is not
acceptable. On any pixel mismatch, self-pin with
`EC_AV1_GATE_DUMP=/tmp/claude-1000/rectwire-flake-N.obu`, then localize by
comparing the msac RANGE (never `tell()` — baselines differ by fixed constants
between decoders) from our `EC_AV1_TRACE` output against the instrumented
`~/.cache/aom-oracle/build/aomdec` under `EC_TRACE=1`. Ranges equal up to the
bad block means a prediction defect; ranges diverging means a consumption
defect. Do NOT guess-fix.

## Done criteria
Full lib suite stays 226 passed / 0 failed. At least one of the two refusals
narrowed or lifted with pixel-exact evidence, and the gate that proves it
fires and HARD-ASSERTS its firing count (a gate that cannot fire the feature it
names measures nothing — the 8x8-OBMC gate is the cautionary example).
REPORT `lanes/rectwire-r1.report.md`, VERDICT on the FIRST line, with the
before/after refusal counts and the firing count.
