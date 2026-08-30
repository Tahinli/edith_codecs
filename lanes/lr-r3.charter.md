# lane-lr r3 — gate the symbols, then apply the filters

Worktree `/home/tahinli/Documents/Code/Rust/edith_codecs-lr`, branch `lane-lr`.
Read ONLY the r2 section of `lanes/lr.report.md` — it is a typing plan, not a
re-derivation task. Do not re-read libaom for what it already records
(class `worker-cap-spent-reading`).

## State — the symbols are read
r2 landed and verified (suite 235 passed / 0 failed, 181 s — 232 baseline plus
three new pinned roundtrip tests):
- `crates/ec-av1/src/restoration.rs` (new): the msac-flavoured subexp/recenter
  port (`decode_subexp_msac`, `decode_unsigned_subexp_with_ref_msac`,
  `decode_signed_subexp_with_ref_msac`, `ns_msac`), `WienerInfo` / `SgrprojInfo`
  / `UnitFilter`, the `SGR_PARAMS` table, `read_wiener_filter` /
  `read_sgrproj_filter` / `read_lr_unit` / `read_lr`, and `RestorationGrid`.
- `decode.rs`: both tile functions take `lr: &LoopRestorationParams` and call
  `read_lr` once per superblock before `decode_partition`; the public wrappers
  pass `&LoopRestorationParams::default()`.
- `stream.rs`: both call sites thread `&header.loop_restoration`; the refusal
  now names the true remaining gap ("symbols read, filters not applied").

## Order — COMMIT AFTER EVERY GREEN STEP
1. **The gate that proves stage 2 actually worked.** A real aomenc
   `--enable-restoration=1` stream must survive the partition walk and fail
   only on the new refusal — before this, "the symbols are read correctly" is
   unproven. Hard-assert a firing count with a thread-local `Cell<usize>` in
   `restoration.rs`'s `read_lr_unit`. Remember the shape of the trap r2's
   sibling lane hit: a per-superblock symbol needs a fixture with more than one
   superblock, or it can never fire. COMMIT.
2. **Wiener.** A new `apply_loop_restoration` pass after `apply_cdef` in
   `decode.rs`, driven by `RestorationGrid`. The 3-pixel stripe boundary
   save/restore (libaom's `rlbs`) is the classic trap — 64-row stripes, and the
   filter reads rows the next stripe has already overwritten unless you save
   them. COMMIT.
3. **Self-guided**, incl. the box-sum radii and `SGR_PARAMS`. COMMIT.
4. **Switchable**; drop the stream.rs refusal; the gate asserts Ok, never an Err.
   COMMIT.

Ordering fact a sibling lane established, which you need for step 2: libaom's
`decodeframe.c` calls `superres_post_decode()` BETWEEN `av1_cdef_frame` and
`av1_loop_restoration_filter_frame` — **superres runs before loop restoration**.
Not load-bearing while superres is refused here, but get the order right now.

## Gate rules
`EC_AV1_REQUIRE_AOMENC=1` on every run (a missing oracle must FAIL, not SKIP);
`-t <seconds>` on every ffmpeg generate; fixtures through
`gradients_source(seed, w, h, tail)`; aomenc
`--threads=1 --row-mt=0 --enable-restoration=1`; firing counts are HARD asserts.

## Note for the merge
Main carries two guards you do not have: `gate_coverage.rs` (pins the aomenc
tools no gate exercises — `enable-restoration` is one gate away from that list)
and `refusal_inventory.rs` (pins every decode-path refusal string, so adding or
renaming one fails until the list is updated — and r2 RENAMED the LR refusal).
Report the refusal strings you add, rename or remove, verbatim.

## Hard rules
`export CARGO_TARGET_DIR=$HOME/.cache/cargo-target-lr`; foreground builds
`nice -n 19 cargo ... -j4`; every `cargo test` a timeout >= 600000 ms (the suite
runs ~3 min). Sibling worktrees (edith_codecs, -chroma, -realworld, -superres,
-tiles, -palette) have live agents — never build in or edit them. Never push,
never merge, never touch `main`. 75-turn cap, does not reset: commit at every
green step. Update `lanes/lr.report.md` with an r3 section, VERDICT first.
