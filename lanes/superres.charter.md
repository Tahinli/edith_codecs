# lane-superres — superres decode (frame_size_with_refs / horizontal upscale)

Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-superres`, branch
`lane-superres`, off main 87a1e34.

## Goal
Make the decoder handle a stream aomenc produced with `--superres-mode=`
(fixed and/or qthresh) end to end, pixel-exact vs libaom:
1. header side: `use_superres` / `superres_denom` are read into the frame
   header (check whether `crates/ec-av1-syntax` already parses them — see
   `crates/ec-av1-syntax/src/frame.rs:518` — and whether the DECODE path in
   `crates/ec-av1/src/stream.rs` refuses or silently ignores them);
2. decode side: the frame is coded at `frame_width` (the DOWNSCALED width),
   every block/MV/loop-filter/CDEF operation happens at that width, and only
   after loop restoration is the frame upscaled horizontally to
   `upscaled_width` by the spec's linear-filter upscaler;
3. reference frames are stored UPSCALED, and `frame_size_with_refs` /
   MV scaling for inter prediction across a size change must be right.

## Anchors
- Spec: 5.9.8 `superres_params`, 7.16 `upscaling process`, 5.9.7
  `frame_size_with_refs`, 7.11.3.3 (motion vector scaling / scaled reference).
- libaom (source tree under `~/.cache/aom-oracle`):
  `av1/common/resize.c` — `av1_upscale_normative_rows`,
  `av1_upscale_normative_and_extend_frame`, `upscale_normative_rect`,
  and the `av1_resize_filter_normative` coefficient table (the 8-tap
  `SUPERRES_FILTER` table with `RS_SUBPEL_BITS`/`RS_SCALE_SUBPEL_BITS`);
  `av1/decoder/decodeframe.c` `setup_superres` / `superres_post_decode`.
- Note the ORDER: superres upscaling happens AFTER deblock+CDEF and
  AFTER loop restoration in libaom's pipeline
  (`av1_loop_restoration_filter_frame` runs on the downscaled frame using the
  saved pre-upscale buffer). Get this order from the source, not from memory.
- Writer side already exists and refuses: `crates/ec-av1/src/frame.rs:309-316`
  writes `use_superres` and refuses a denom other than 8. Leave the writer's
  refusal in place unless you also implement the encoder-side downscale — that
  is OUT of scope for this lane; this lane is DECODE.
- `crates/ec-av1/src/stream.rs` holds every decode refusal and every gate.
  `grep -n unsupported crates/ec-av1/src/stream.rs` to see the shape.

## Step 0 — measure before building
Encode a real fixture with `--superres-mode=1 --superres-denominator=<n>`
(check `aomenc --help | grep -i superres` for the actual flag spelling in
v3.13.3) and run it through our decoder as it stands today. Report in the
report file: does it refuse (with what exact string), desync, or decode wrong?
That answer decides whether stage 1 is a refusal removal or a bug fix. If the
header path already silently ignores superres and produces wrong pixels, that
is a SILENT WRONGNESS defect and must first become an explicit refusal, in its
own commit, before the implementation lands.

## Staging — commit after EVERY green milestone
1. Header + explicit refusal by accurate name. COMMIT.
2. Upscaler implemented and unit-pinned: a `#[test]` that feeds a known row to
   our upscaler and compares against values captured from libaom's
   `av1_upscale_normative_rows` (add an instrumentation rung to
   `scripts/instrument-aom-oracle.sh` if you need to capture them — follow the
   existing rungs' shape exactly: env-gated, silent when unset, idempotent,
   wrapper-around-impl — then rebuild with `scripts/build-aom-oracle.sh`).
   COMMIT.
3. Full-frame gate: decode an aomenc superres stream pixel-exact vs
   `aomdec --rawvideo`. COMMIT.
4. Inter frames referencing a differently-sized reference (superres-mode=2
   /qthresh, so denom varies frame to frame). COMMIT.

## Gate (mandatory)
- Follow the existing gate shape in `crates/ec-av1/src/stream.rs`.
  `EC_AV1_REQUIRE_AOMENC=1` must be set when you run tests: a missing oracle
  must FAIL, never SKIP.
- Bound every ffmpeg `generate` with `-t <seconds>`.
- Build the fixture through the existing `gradients_source(seed, w, h, tail)`
  helper — ffmpeg's `gradients` source ignores its seed, so a hand-written
  `gradients=size=` string makes the gate non-reproducible.
- aomenc: `--threads=1 --row-mt=0`.
- Hard-assert a firing count via a thread-local `Cell<usize>` counter like the
  existing `*_HITS` in decode.rs (they are thread-local now, NOT atomics).
  A gate that cannot prove superres actually fired is vacuous
  (CLASS `gate-blind-to-feature`).
- Refusals inside the gate are FORBIDDEN once the stage removing them lands.

## Hard rules
- Own build dir: `export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-superres`.
  Foreground builds only, `nice -n 19 cargo ... -j4`. Never build in another
  worktree, never touch a sibling worktree's files.
- Suite: `nice -n 19 cargo test -p ec-av1 --lib` (baseline 232 passed / 0
  failed, ~67 s). Must stay green.
- NEVER push, never merge, never touch `main`. Commit on `lane-superres` only.
- 75-turn cap: **commit after every green milestone**; near the cap commit
  whatever compiles as `wip(av1): ...` and write `lanes/superres.report.md`.
- Refuse-by-name rather than desync. Never claim in a refusal string that the
  encoder cannot emit a case unless you proved it.
- Write `lanes/superres.report.md`: what landed, gate name + firing count,
  remaining refusal strings verbatim, next lever.
