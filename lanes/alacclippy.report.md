# Lane alacclippy: workspace clippy clean under -D warnings

## Goal
`cargo clippy --workspace --all-targets -- -D warnings` must pass. ec-av1, ec-h265 and ec-h264 have their own gates and reported no lints. ec-alac was the known failure.

Base sha: 8fb9fb6c (main). Branch: lane/alacclippy.

## Changes
All fixes are behavior-preserving. No `#[allow]` was added.

- `chunks_exact(N)` with a constant N, replaced with `as_chunks::<N>().0.iter()`. This yields the same full chunks, as `&[u8; N]` instead of `&[u8]`. About 30 sites:
  - crates/ec-alac/src/encode.rs (2, the original failure)
  - crates/ec-alac/tests/encode.rs
  - crates/ec-mp3/tests/decode_matrix.rs, crates/ec-mp3/tests/encode_matrix.rs
  - crates/ec-probe/src/decoder.rs, crates/ec-probe/src/tags.rs, crates/ec-probe/tests/library.rs
  - shims/image/src/lib.rs
  - shims/opus-rs/src/lib.rs (const-generic `{ FRAME * N }` form)
  - shims/oxideav-ac3/src/lib.rs, shims/oxideav-ac3/tests/edith_surface.rs
  - shims/symphonia/tests/vorbis_timing.rs (also `ts: packet.pts.value() as i64` became `packet.pts.value()`, a same-type cast)
  - tools/ec-bench/src/main.rs (`s.as_chunks::<2>()` in the probe helper)
- `bits.len() % 8 != 0` became `!bits.len().is_multiple_of(8)` (crates/ec-mp3/src/huffman.rs).
- tools/ec-bench/src/main.rs:
  - manual `for`/`if return` search became `found.into_iter().find(..)`
  - two nested `if let` became let-chains (edition 2024)
  - `loop { match next_packet { Ok => +1, Err => break } }` became `while reader.next_packet().is_ok()`

Lints in ec-av1, ec-h265, ec-h264: none reported. Left alone.

## Verify
- `cargo clippy --workspace --all-targets --keep-going -- -D warnings` on server host c (via scripts/remote-run.sh alacclippy --host auto), final run: exit 0, no warnings or errors.
- `cargo test --release --no-fail-fast -p ec-alac -p ec-mp3 -p ec-probe -p oxideav-ac3 -p image -p opus-rs -p symphonia` on server: all pass except one test (below).
  - ec-alac: lib 2 passed, encode 8 passed, oracle 3 passed
  - ec-mp3: lib 22 passed, decode_matrix 1 passed, encode_matrix 5 passed, 2 ignored
  - ec-probe: lib 7 passed, library 7 passed, matrix 6 passed
  - image: 4 passed
  - opus-rs: 6 passed
  - oxideav-ac3: lib 3 passed; tests/edith_surface FAILED (1 failed)
  - symphonia: edith_surface 6 passed, vorbis_timing 2 passed
- `cargo test --release -p ec-bench` run locally: exit 0 (0 tests). The server lacks libva (`-lva`, `-lva-drm`), so ec-bench cannot link there.
- Pre-existing failure, not caused by this lane: `oxideav-ac3 --test edith_surface::the_replicas_open_path_lands_on_audible_pcm` panics with `AC-3 decode failed: Unsupported("a second syncframe in the tail of the packet ...")`. It fails identically with the whole shims/oxideav-ac3 crate reverted to main (A/B run on server), so the cause is in the decoder or its fixture input, not in the clippy edits.

## Left
- The oxideav-ac3 edith_surface test failure is a real pre-existing failure, outside this lane's scope.
- Formatting: the touched files are not rustfmt-clean on main either (long lines), so surrounding style was kept and no reformat was done.
