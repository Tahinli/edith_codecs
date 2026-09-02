# lane-mcomp64 r1 — inferred COMPOUND_DIFFWTD at 64x64

## What changed
- `crates/ec-av1/src/decode.rs:13376` — `wedge_bsize` is now `Option<usize>`; `None`
  (side 64) means libaom codes **no** `compound_type` symbol and infers
  `COMPOUND_DIFFWTD` (`read_compound_type`, decodemv.c 1634-1656; guard is
  `is_interinter_compound_used(COMPOUND_WEDGE, bsize)` →
  `av1_wedge_params_lookup[BLOCK_64X64].wedge_types == 0`). Only the 1-bit
  `mask_type` literal follows. Refusal deleted.
- `crates/ec-av1/src/decode.rs:759` — new `DIFFWTD_INFERRED_HITS` / `diffwtd_inferred_hits()`:
  strictly the inferred (no-symbol) arm, narrower than `DIFFWTD_HITS`.
- `crates/ec-av1/src/refusal_inventory.rs:66` — refusal string removed (47 → 46).
- `crates/ec-av1/src/gate_coverage.rs:252` — `NEVER_ON_10BIT` is now **empty**: the new
  10-bit arm is the first recipe to spell `--enable-diff-wtd-comp=1` *and* fire it.
- `crates/ec-av1/src/stream.rs:9683` — new gate
  `run_diffwtd_inferred_64x64_gate` + 8-bit and 10-bit `#[test]` arms.

No new mask/blend code was needed: `mc::diffwtd_mask` already carries the `bd - 8`
round term and `mc::blend_masked_compound` already takes block dims + the subsampled
chroma read, so both are size- and bit-depth-generic. The `comp_group_idx` context
(`get_comp_group_idx_context`) and `record_compound_ctx_rect_mi` already gather/write
over the whole `side` span, so 64x64 needed no context work either.

## Scope note (charter asked for more shapes)
The charter listed 64x32/32x64/64x16/16x64/8x32/32x8. This decoder's square inter path
(`decode_inter_block(.., side, ..)`, plus the 8x8 leaf `decode_inter_block8`) is the ONLY
path that reaches `read_compound_type`; every rectangular inter shape is refused one level
up by `"a non-skip rectangular (HORZ/VERT/HORZ_B) strip needs rectangular residual coding"`
(ledger constraint, decode.rs). So 64x64 is the only wedge-less size reachable, and code for
the other shapes would be unreachable and ungateable. `deferred: rect wedge-less shapes —
unreachable until rect inter residual coding exists — unblocked by the rect-inter lane`.

## Gate
`a_real_aomenc_stream_with_a_diffwtd_compound_64x64_block_decodes_pixel_exact` and
`a_real_aomenc_10bit_stream_with_a_diffwtd_compound_64x64_block_decodes_pixel_exact`
(`crates/ec-av1/src/stream.rs`). 128x128, 16 frames, 3 cq × 2 mandelbrot+noise variants.
Recipe pins every block to the inferred size (`--sb-size=64 --max-partition-size=64
--min-partition-size=64`) with `--enable-masked-comp=1 --enable-diff-wtd-comp=1
--enable-dist-wtd-comp=0` so `comp_group_idx == 1` has no other masked choice; every
frame's Y/U/V compared against ffmpeg; a decode error or mismatch FAILS (the old refusal
string is explicitly forbidden); hits snapshotted per attempt and accumulated only for
attempts that decoded AND pixel-compared (class `counter-from-refused-stream`).

Run:
```
CARGO_TARGET_DIR=$HOME/.cache/cargo-target-mcomp64 EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 \
  nice -n 10 cargo test -p ec-av1 --lib -j3 diffwtd_compound_64x64 -- --nocapture --test-threads=1
```
EVIDENCE: $HOME/.cache/cargo-target-mcomp64 test stdout | 2 gates, 6 aomenc encodes each, every frame Y/U/V vs ffmpeg | 8-bit 6/6 pixel-exact diffwtd_inferred_hits=79; 10-bit 6/6 pixel-exact diffwtd_inferred_hits=51; 2 passed 0 failed

## Film probe
EVIDENCE: scratchpad/hg-head.obu (Hunger Games 10-bit head) | `cargo run --release -p ec-av1 --example decode_probe` on b92150f, and on b92150f + cherry-picked lane-fi8 2d5c425 (scratch worktree, main lacks fi8) | on b92150f alone: `filter intra on a HORZ/VERT strip` (fi8's refusal, hit first); with fi8: `a Golomb tail longer than this decoder reads` — the masked-compound-64x64 stop the charter recorded on lane-fi8 is GONE.

`open: the new Hunger-Games stop "a Golomb tail longer than this decoder reads" is not
proven to be a genuine next capability rather than a downstream desync of the newly
decoded 64x64 masked block; the gate says the blend and symbol order are pixel-exact on
real aomenc streams, but the film is not decoded end-to-end. Unblocked by a lane that
bisects that frame against the instrumented aomdec range ladder.`

## Suite
See "Suite result" below (full `cargo test -p ec-av1 --lib` under systemd-run, log
`$HOME/.cache/mcomp64-suite.log`).

## Suite result
`systemd-run --user --unit=mcomp64-suite-... -p MemoryMax=10G` →
`EC_NOMEMGUARD=1 EC_AV1_REQUIRE_AOMENC=1 CARGO_TARGET_DIR=$HOME/.cache/cargo-target-mcomp64
nice -n 10 cargo test -p ec-av1 --lib -j3`, log `$HOME/.cache/mcomp64-suite.log`.
EVIDENCE: $HOME/.cache/mcomp64-suite.log | full ec-av1 lib suite on b92150f | `test result: ok. 335 passed; 0 failed; 31 ignored; finished in 405.30s`
Sibling gates re-run inside that suite and green: masked_compound / wedge / interintra /
wedge-interintra / compound_* / inter_sequence* / refusal_inventory / gate_coverage.

## Same-shape sweep (class: alphabet collapses at a size)
The only other size-conditional wedge alphabet is interintra's `wedge_interintra`
(`is_interintra_wedge_used`); interintra itself is allowed only for BLOCK_8X8..BLOCK_32X32,
every one of which has `wedge_types > 0`, so no second instance exists on this decode path.
The 8x8 leaf (`decode_inter_block8`) is hard-coded to `wedge_bsize == 3`, correct by
construction.
