# lane-palette — palette reconstruction, then intra block copy

Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-palette`, branch
`lane-palette`, off main (c4904ee or later).

## Goal
Two of the five coding tools that **no gate in this repository exercises**
(main's `crates/ec-av1/src/gate_coverage.rs` pins the set) are palette and
intrabc. Both have their syntax consumed and both refuse the moment a block
actually uses them. Make them decode.

## Where the refusals are
- Palette, key-frame reader — `crates/ec-av1/src/decode.rs:2484-2494`:
  `"a block that actually uses a palette (Y) -- reconstruction is out of scope"`
  and the `(UV)` twin. The `palette_y_mode` / `palette_uv_mode` symbols ARE
  read; only a nonzero size refuses.
- Palette, inter-frame intra-block reader — `decode.rs:7795-7805`, the same two
  strings. Note the comment there: `palette_mode_ctx` / `palette_uv_mode_ctx`
  are hardcoded 0, which is only safe while a nonzero neighbour `palette_size`
  is impossible. **The moment palette blocks decode, those contexts become
  live** — `av1_get_palette_mode_ctx` (pred_common.h) reads the neighbours'
  own `palette_size[0] > 0`. Class `context-read-from-one-cell` and
  `cdf-row-held-constant` both apply: fix the context in the SAME round that
  removes the refusal, or the gate will desync in a way that looks like a
  table bug.
- Intrabc — `decode.rs:2408-2416`:
  `"a block that actually uses intrabc (this decoder never reconstructs one)"`.
- Also `decode.rs:1940-1947`: a HORZ/VERT intra strip in a screen-content frame
  refuses because `palette_bsize_ctx` is keyed on a single side. That refusal
  is in scope for stage 3.

## Anchors
- Spec 5.11.13 `palette_mode_info`, 5.11.46 `palette_tokens`, 5.11.50
  `get_palette_cache`, 7.11.4 `palette prediction process`; 5.11.13
  `read_intrabc_info`, 6.10.19, 7.11.3.x for the intrabc MV rules.
- libaom, source tree under `~/.cache/aom-oracle`:
  `av1/decoder/decodemv.c` `read_palette_mode_info` (line ~567),
  `read_palette_colors_y` / `read_palette_colors_uv`, and the delta-coded
  colour reads; `av1/common/pred_common.h` `av1_get_palette_mode_ctx` and
  `av1_get_palette_bsize_ctx`; `av1/decoder/decodetxb.c` /
  `av1/common/blockd.h` for the colour index map; `av1_get_palette_cache`
  (av1/common/reconintra.c) for the above/left colour cache;
  `read_intrabc_info` (decodemv.c ~693) and `av1_find_ref_dv` for intrabc's
  forced DV predictor and its validity bounds.
- `crates/ec-av1/src/cdf_state.rs:425-433` already carries `palette_y_mode`,
  `palette_y_size`, `palette_uv_mode`, `palette_uv_size` with defaults and
  counter resets. The colour-index-map CDFs (`palette_y_color_index` /
  `palette_uv_color_index`, indexed by palette size and a neighbour context)
  are NOT there — adding them is four wiring sites: struct field, defaults
  array, save/restore, per-frame counter reset (`cdf_state.rs` `reset2`/`reset3`
  are length-generic and save/restore is a whole-struct Clone, so the defaults
  array is what needs hand-checking; still verify the counter reset covers the
  new tables — class `cdf-counter-not-reset`).

## Staging — COMMIT AFTER EVERY GREEN MILESTONE
1. **Palette Y.** Colour cache from the above/left neighbours, delta-coded
   colours, the colour index map (wavefront diagonal scan, the
   `palette_color_index` symbol per pixel with its neighbour context), and
   reconstruction. Fix `palette_mode_ctx` / `palette_uv_mode_ctx` to read the
   neighbours' real palette sizes in this same commit. COMMIT.
2. **Palette UV.** COMMIT.
3. **The rect-strip refusal** at decode.rs:1940 — key `palette_bsize_ctx` on
   both sides the way `av1_get_palette_bsize_ctx` does
   (`num_pels_log2_lookup[bsize]`). COMMIT.
4. **Intrabc.** The DV predictor, the wavefront/delay validity bounds (a DV may
   only reference already-reconstructed pixels far enough behind the current
   superblock), and prediction from the current frame's own reconstruction.
   COMMIT.

## Gate (mandatory, in `crates/ec-av1/src/stream.rs` beside the existing gates)
- Copy an existing gate's shape. `EC_AV1_REQUIRE_AOMENC=1` must be set on every
  test run, so a missing oracle FAILS rather than SKIPs.
- Bound every ffmpeg `generate` with `-t <seconds>`.
- Build fixtures through the existing `gradients_source(seed, w, h, tail)`
  helper — ffmpeg's `gradients` source ignores its own seed. Palette and intrabc
  are screen-content tools: aomenc will only choose them on flat, few-colour,
  repetitive content, so you will likely need a synthetic source
  (few-colour test pattern, repeated tiles) rather than a gradient. Whatever you
  use, it must be deterministic — hash the fixture across two runs and prove it.
- aomenc `--threads=1 --row-mt=0 --enable-palette=1` (and `--enable-intrabc=1`
  for stage 4), plus `--tune-content=screen` if that is what makes the encoder
  actually pick the tool.
- Hard-assert firing counts via thread-local `Cell<usize>` counters in decode.rs
  matching the existing `*_HITS` (thread-local, NOT atomics): palette blocks
  reconstructed > 0, intrabc blocks > 0. A gate that cannot prove its feature
  fired is vacuous (class `gate-blind-to-feature`).
- **Main's `gate_coverage.rs` guard will fail by design** once your gate enables
  these tools: its `NEVER_EXERCISED` list names `enable-palette` and
  `enable-intrabc`. Deleting those two entries is part of landing this work.

## Method
CLASS `compare-range-not-tell`: compare the msac RANGE against the oracle after
each element, never `tell()`. Oracle at `~/.cache/aom-oracle`; env-gated rungs
`EC_TRACE=1` (partitions), `EC_TRACE_COEFF=1`, `EC_TRACE_MODE=1` (inter + intra
mode info), `EC_AV1_PREFILT_DUMP=<prefix>` (per-frame pre-filter recon — the
right rung for separating a prediction bug from a filter one). Add a rung with
`scripts/instrument-aom-oracle.sh` + `scripts/build-aom-oracle.sh` in the
existing shape (env-gated, silent when unset, idempotent, wrapper-around-impl).
CLASS `equal-range-means-unread`: reference range unchanged where ours moves =
we read a symbol it never wrote; theirs moves and ours does not = we skipped one.

## Hard rules
- `export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-palette`. Foreground
  builds, `nice -n 19 cargo ... -j4`. Give every `cargo test` a timeout of at
  least 600000 ms — the suite runs 3-4 minutes and a 120 s default kills it.
- Sibling worktrees (edith_codecs, -chroma, -realworld, -lr, -superres, -tiles)
  have live agents. Never build in or edit them.
- Baseline: 234 passed / 0 failed on main.
- NEVER push, never merge, never touch `main`. Commit on `lane-palette` only.
- 75-turn cap and it does NOT reset on resume: commit at every green step.
  Stage 1 committed and green is a good round on its own.
- Refuse-by-name rather than desync; never claim in a refusal string that the
  encoder cannot emit a case unless you proved it.
- End with `lanes/palette.report.md`, VERDICT on the first line: what landed,
  gate names + firing counts, remaining refusal strings verbatim, next lever.
