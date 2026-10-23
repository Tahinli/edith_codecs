# Lane vp8guards: malformed VP8 cannot panic the decoder

Base: `main` at `95bc8a76`. Branch `lane/vp8guards`, worktree `~/.cache/wt/vp8guards`.

## Goal

DONE.md "ec-vp8 decode": malformed VP8 input must not panic the decoder in release
builds. Malformed input now yields `ec_core::Error::corrupt`, or the decode returns
normally.

## What changed

1. **Loop filter bounds** (`crates/ec-vp8/src/loopfilter.rs`). `filter_frame_ex` and
   `filter_frame` now return `Result<()>`. The geometry check (plane lengths, strides,
   `mb_info` length) is a real check, done once per frame and returned as
   `Error::corrupt`. The `debug_assert!`s that guarded the edge helpers are gone. The
   per-row `try_into().unwrap()` loads are replaced by `copy_from_slice`. Every tap an
   edge reads lies inside the span the frame check validates (see the comment on the
   check), so no per-pixel test is needed.
2. **Intra panics** (`crates/ec-vp8/src/intra.rs`). `predict_block`, `predict16`,
   `predict8` and `predict4` return `Result`. The two `panic!` arms are
   `Err(Error::corrupt(..))`. The error propagates with `?` through
   `reconstruct_intra_mb` and `reconstruct_inter_mb` to `decode`.
3. **Other reachable panics in ec-vp8**:
   - `mc.rs`: the reference read origin was guarded only by `debug_assert`. An
     unclamped split MV could index past the bordered plane. New `clamp_read_origin`
     brings the origin back to the nearest in-range position, once per block. For
     conforming streams the origin is always in range, so output is unchanged. Clamped
     reads land in border replication, which gives the same samples.
   - `decode.rs`: `expect` on the key-frame dimensions and on the reference slot became
     `Error::corrupt`. `run_loopfilter` propagates its `Result`.
   - Left as-is, because no input can reach them: `unreachable!` in `modes.rs`
     (tree-produced modes), `ymode_as_bmode` in `decode.rs`, `mc.rs` `bw` match,
     `bool.rs` encoder carry, and `i32::try_from(col).unwrap()` in `modes.rs`.
4. **Clippy** (`--all-targets -D warnings`). Now clean. Fixes were mostly
   mechanical (`div_ceil`, `is_multiple_of`, `clamp`, `contains`, `as_chunks`).
   Index loops over parallel arrays and too-many-argument C-port signatures carry
   `#[allow]` with a reason comment.
5. **New test** `crates/ec-vp8/tests/malformed_no_panic.rs`. Each of the 12 IVF
   fixtures is decoded truncated at 10 positions and with 50 single-byte corruptions
   (fixed splitmix64 seed `0x5EED_0F08`). Each decode runs under `catch_unwind`.
   The test does not depend on the decode result, only on the absence of a panic.
6. **Infrastructure fix** `scripts/remote-run.sh`. The sync exclude was `--exclude=/fixtures/`
   (directory form). It copied the worktree's `fixtures` symlink, which points at a path
   that does not exist on the server, so every fixture test failed with "fixtures missing".
   The exclude is now `--exclude=/fixtures`. The stale symlink was removed on both hosts.
   The hosts' own `~/fixtures` link is recreated by the existing `ln -s` line.

Test-only changes: intra tests that expected a panic now assert `is_err()`. The
`filter_frame` test calls now `unwrap()`.

## Verify

- `scripts/remote-run.sh vp8guards -- cargo clippy -p ec-vp8 --all-targets -- -D warnings`:
  clean (exit 0).
- `EC_REQUIRE_FIXTURES=1 scripts/remote-run.sh vp8guards -- cargo test -p ec-vp8 --release`:
  all pass (exit 0).
  - lib unit tests: 57 passed
  - `header_parse`: 4 passed
  - `keyframe_exact`: 4 passed (includes the WebP still)
  - `malformed_no_panic`: 2 passed (about 13 s)
  - `stream_exact`: 3 passed (includes `decode_stream_matches_ffmpeg_on_every_fixture`,
    byte-exact on all 12 fixtures)
- Check that the new test has teeth: on the pre-change `src/` it fails with 2 panicking
  inputs out of 600 (`altref-160x96.ivf` corruption 22, `clip-obs-320x192.ivf`
  corruption 43). With the change it passes.

## Left

- The no-panic guarantee is empirical for ec-vp8's decode path. The test covers 600
  deterministic inputs, not every byte position. A property-based or fuzz harness
  would be stronger.
- The reachable-panic audit covered `panic!`, `unreachable!`, `unwrap`, `expect` and
  `debug_assert`-guarded indexing in ec-vp8 `src`. Unguarded indexing with
  input-derived indices that is not in those forms was not audited line by line.
