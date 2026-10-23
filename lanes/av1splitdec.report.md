# av1splitdec: decode.rs module split (av1split D1-D21)

Goal: mechanical module extraction of `crates/ec-av1/src/decode.rs` (61,650 lines) so that no
decode source file exceeds 5,000 lines (DONE.md / `lanes/av1split.plan.md` section 2), with no
behaviour change.

Base: `main` @ 322837a2 (merged into the lane as c60b28ff). Gated head: `90a7b62f`.
Plan steps P0 (directory-aware source reader) was already on main (028fc46e), so the
`include_str!` scanners already read `decode/` as a directory.

## Steps

| Step | What moved | File |
|---|---|---|
| D1 | `decode.rs` -> `decode/mod.rs` (git mv, 0 lines) | `decode/mod.rs` |
| D2 | `mod tests`, `mod tx4x8_table_tests`, `mod obmc_skip_chroma_tests` | `tests.rs`, `tx4x8_table_tests.rs`, `obmc_skip_chroma_tests.rs` |
| D3 | hit/census counter items (name-based) | `counters.rs` |
| D4 | `mod cdef_simd`, `mod deblock_simd` | `cdef_simd.rs`, `deblock_simd.rs` |
| D5 | `decode_inter_frame_tile*` | `frame_tile.rs` |
| D6 | `grid_stamp_rect`, `decode_inter_block8` | `inter_block8.rs` |
| D7 | `decode_inter_sub8_split4`, `decode_intra_sub8_leaf`, `decode_inter_sub8_rect2` | `sub8_inter.rs` |
| D8a | `decode_inter_block`: its intra `else` arm (~1,300 lines) extracted to `decode_inter_block_intra_arm`; the arm returns the locals the tail reads and `Ok(None)` for its early return | `inter_intra_arm.rs` |
| D8b | `decode_inter_block` (about 4,430 lines after D8a) | `inter_block.rs` |
| D9 | switchable interp / ref / OBMC / compound helpers | `inter_ctx.rs` |
| D10 | `decode_key_frame_tile*` region | `key_tile.rs` |
| D11 | `replay_*`, `cdef_band` | `replay.rs` |
| D12 | deblock filters, scratch, filter-replay capture | `deblock.rs` |
| D13 | block tx-size readers, inter shapes, edge params | `inter_shapes.rs` |
| D14 | CDEF constants and kernels | `cdef.rs` |
| D15 | sub-8x8 chroma leaves, split4, rect8 leaf | `leaf_sub8.rs` |
| D16 | 128-rect block, 8x8 leaf, sub-8 intra mode | `leaf8.rs` |
| D17 | intra mode/angle readers, CFL, plane reader, `decode_block` | `block.rs` |
| D18 | rect4 and 16-wide blocks | `rect4.rs` |
| D19 | rect intra/intrabc blocks, `decode_leaf_rect` | `rect.rs` |
| D20 | coefficient readers, `Neighbours`, filter-intra and palette | `coeffs.rs` |
| D20b | post-filter pipeline (CDEF / LR drivers, pipeline statics); not in the plan, added so `mod.rs` fits | `post_filter.rs` |
| D21 | final checks: lint cleanups (see below), all files under 5,000 lines | |

The plan's base line ranges no longer matched after the first moves, so every step selected its
region by first and last item name. Each region was contiguous in the file at the time it moved.

## Line counts (`crates/ec-av1/src/decode/`, after D21)

| File | Lines |
|---|---:|
| `rect.rs` | 4842 |
| `mod.rs` | 4767 |
| `coeffs.rs` | 4541 |
| `counters.rs` | 4460 |
| `inter_block.rs` | 4437 |
| `rect4.rs` | 3865 |
| `tests.rs` | 3828 |
| `frame_tile.rs` | 3658 |
| `block.rs` | 3587 |
| `key_tile.rs` | 3086 |
| `sub8_inter.rs` | 3043 |
| `leaf_sub8.rs` | 3040 |
| `inter_ctx.rs` | 2992 |
| `inter_block8.rs` | 2973 |
| `leaf8.rs` | 2087 |
| `inter_shapes.rs` | 1714 |
| `inter_intra_arm.rs` | 1359 |
| `deblock.rs` | 1024 |
| `post_filter.rs` | 825 |
| `replay.rs` | 596 |
| `deblock_simd.rs` | 438 |
| `cdef.rs` | 388 |
| `cdef_simd.rs` | 242 |
| `tx4x8_table_tests.rs` | 89 |
| `obmc_skip_chroma_tests.rs` | 27 |

Largest: `rect.rs` 4,842, `mod.rs` 4,767. All under 5,000. `decode/` previously held one 61,650-line file.

## Non-mechanical changes (what is not a pure move)

- Visibility only: moved items that the rest of the module uses became `pub(super)`, struct
  fields and impl methods used across files became `pub(super)`, thread-local statics too. The
  module `mod.rs` re-exports moved items with `pub use` / `pub(crate) use` (chosen to match the
  widest visibility in each moved region), so `ec_av1::decode::*` paths are unchanged.
- D8a is the only change to code shape. The `else` arm body is unchanged apart from the
  signature and 4-space dedent. Its early `return Ok(())` became `return Ok(None)` and the caller
  does `return Ok(())` on `None`. Its outputs are returned in a tuple (`IntraArmOut`) and
  destructured back into the caller's deferred locals. The arm reads the same values it read
  inline: the locals it reads are passed by value or by `&mut` as before. `has_chroma`,
  `is_inter` and `palette_record`'s incoming value were dropped because the arm never reads them
  (its `palette_record` is always assigned before use). This is covered by the test gate below.
- Source-scan gates. Six gates in `decode/tests.rs`, `stream.rs` and `refusal_inventory.rs` read
  decode source as text and match `\nfn name(` headers and function bodies. The split broke them
  in three ways, all fixed without changing what they assert:
  1. `test_source::read_module` (the shared helper) rewrites column-0 `pub(super) fn ` headers to
     `fn ` before the text is scanned, so the scanners see the same headers as on `main`.
  2. `decode/tests.rs` scope gate: the 1:4 test text can occur first in another file (concatenation
     is in path order), so the gate now takes the first occurrence whose own function holds the
     dispatch call; guard, test and dispatch must still share that function.
  3. `stream.rs` counts sites in `decode_inter_block` and `decode_inter_block_intra_arm` together,
     since D8a moved some of those sites into the helper.
  Also, `pub(super)` was reverted to plain `fn` for moved functions that no other decode file uses.
- Lint fixes made during D21: dangling doc blocks joined (6 places); the arm helper carries a
  reasoned `#[allow(clippy::too_many_arguments, clippy::needless_late_init)]`.

## Verify

- `scripts/remote-run.sh av1splitdec -- cargo check -p ec-av1 --tests` after every step: clean.
  The only warnings are the ones main already has (12 lib, 11 lib-test); no new warnings.
- `cargo check --workspace --all-targets` (server): no errors (all ec_av1 path users still resolve).
- `cargo clippy -p ec-av1 --all-targets` diffed against a `main` worktree
  (`~/.cache/wt/av1split-base`, detached at 322837a2): **no new clippy warnings**; 6 fewer
  (`empty line after doc comment`). Clippy `-D warnings` is not clean on `main` either (322
  warnings there, 316 here), so that gate cannot pass on this crate as it stands.
- Test gate (ORCHESTRATION gate, on host b):
  `EC_MEMGUARD_MAX=6G cargo test --release -p ec-av1 -- --test-threads=4`.
  Required: 807 passed, 49 ignored, 0 failed.

Run on host b, commit `90a7b62f` (the code under the gate):
`EC_MEMGUARD_MAX=6G cargo test --release -p ec-av1 -- --test-threads=4` ->
`test result: ok. 807 passed; 0 failed; 49 ignored; 0 measured` (lib, finished in 2897.70s),
plus a second binary `4 passed; 0 failed`. Required 807 passed, 49 ignored, 0 failed: met.

An earlier gate on the pre-fix commit `fb7cec17` had 801 passed, 6 failed (source-scan gates, see below).
Those six now pass.

## Left

- Out of scope for this lane (decode.rs only), still over 5,000 lines on `main`:
  `stream.rs` (57,178), `encode.rs` (23,771), `tile.rs` (11,516), `qm.rs` (7,046),
  `encoder.rs` (6,503), `cdf.rs` (5,519). `lanes/av1split.plan.md` sections 5c-5g cover them.
- `rustfmt --check` already fails on `main`'s `decode.rs`; the moved code is not reformatted.
