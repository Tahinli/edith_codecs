# lane-sbpart r11 — per-plane quantizer deltas are read and thrown away

At 40225bc. r10 hit its cap without committing; I committed its `EC_SBPART_DUMP64`
trace, stripped its throwaway transform probe, and moved its scratch fixtures to
`fixtures/sbpart/`. Its report is the task notification, not a file — the facts
you need are below.

## What r10 established, and what I confirmed myself
The defect is **not** in `decode_block_rect64`. Seed 42's superblock 0 is a plain
`PARTITION_NONE` 64x64 block going through `decode_block`/`read_plane`, and it
reconstructs px 64..95 of row 0 as flat **81** where ffmpeg and aomdec both give
flat **76**.

- The coefficient is right: level -81, DC-only, `eob=1`, matching aomdec's
  `EC_TRACE_COEFF` exactly. Symbol reading is not the bug.
- The transform is right: r10 hand-derived the whole fixed-point DC-only 64x64
  inverse-DCT chain (`cospi(32)=2896`, `row_shift_wh(64,64)=2`, final column
  shift 4, cross-checked against libaom's `inv_shift_64x64 = {-2,-4}`) and it
  reproduces our own -47 residual exactly. `inverse_dct` / `row_shift_wh` /
  `dequant_coeff_wh` are ruled out.
- So the DEQUANT INPUT is wrong: we need -52, we produce -47.

**I checked r10's suspect myself and it holds.** `crates/ec-av1-syntax/src/frame.rs`
DOES read the per-plane quantizer deltas — `delta_q_y_dc` at line ~1401,
`delta_q_u_dc`/`u_ac` at ~1408, `delta_q_v_*` at ~1414 — so there is no bit
desync. They are stored on the header and then used for **exactly one thing**:
the `lossless[segment_id]` determination at ~1027. Grep `crates/ec-av1/src` for
`delta_q_y_dc`, `delta_q_u_ac`, `delta_q_v_dc` outside the writer and the
lossless check: nothing. `quant.rs`'s `dequant_coeff_wh` takes only
`bit_depth + q_idx + w + h`. **We read the deltas and throw them away.**

Spec 5.9.12: the DC quantizer index is `base_q_idx + delta_q_y_dc` for luma,
and chroma uses `delta_q_u_dc`/`u_ac` (and `v_*` when `separate_uv_delta_q`).
A nonzero `y_dc_delta_q` is a frame-wide, DC-only offset — exactly the shape of
what r10 measured.

## The job
1. **Measure the blast radius FIRST**, before fixing anything: print the five
   deltas for the streams our existing gates encode. If they are nonzero on
   streams whose gates currently pass, then those gates are passing for the
   wrong reason and you have found something bigger than this lane. If they are
   zero everywhere except this recipe, say so — that is the honest scope.
2. Thread the deltas: a per-plane delta into `dc_q`/`ac_q`/`dequant_coeff_wh`/
   `dequant_wh` and every `decode.rs` call site (r10 counted ~15, and its
   `base_q_idx` grep list is the map). Y delta for luma; U/V DC and AC for
   chroma, respecting `separate_uv_delta_q`.
3. Verify with r10's standalone repro — no full-gate re-encode needed:
   `EC_AV1_GATE_DUMP_PIN=fixtures/sbpart/seed42.obu EC_SBPART_DUMP64=1 \
    cargo test -p ec-av1 --lib pinned_sbpart_stream_decodes_pixel_exact -- \
    --ignored --nocapture --test-threads=1`
   Expect row 0 px 64..95 to flip from 81 to 76.
4. Then the full suite, then the lane's own gate.

Do not re-derive the transform math — it is proven correct and ruled out.

If the deltas turn out to be zero for this stream after all, then the suspect is
wrong and the next question is where else `q_idx` could differ: segmentation
qindex, or the `CURRENT_Q_IDX` delta-q state. Say so rather than forcing the
hypothesis.

## Budget discipline
75 turns, no reset on resume. At about turn 55, stop starting new work, COMMIT
(red gate is fine, name it), and write `lanes/sbpart-r11.report.md`. Two rounds
running have ended with no commit and no report file; the report is the one
artifact I cannot reconstruct.

Hard rules: `CARGO_TARGET_DIR=$HOME/.cache/cargo-target-sbpart`; foreground
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms;
`EC_AV1_REQUIRE_AOMENC=1`; aomenc `--threads=1 --row-mt=0 --sb-size=64`. Oracle
rung 11 is yours. Sibling worktrees have live agents — never build in or edit
them. Never push, never merge into main.
