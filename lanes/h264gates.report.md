# Lane h264gates: ec-h264 decode, no panic on malformed input, clippy clean

Base: `95bc8a76` (main). Item: DONE.md "ec-h264 decode", the shared gates line.

## What changed

1. `crates/ec-h264/src/dpb.rs` (`Dpb::store`): the `marking.expect(..)` on a reference
   picture is now `Error::corrupt(..)`, returned with `?` through `end_picture`/`push_nal`.
   Reachable: `nal_ref_idc` is per slice, and the marking is taken from the last slice, so a
   corrupt picture whose first slice is a reference but whose last slice carries no
   `dec_ref_pic_marking` reaches this branch.
   Regression test: `dpb::tests::reference_picture_without_marking_is_corrupt_not_a_panic`
   drives `store` with `marking = None` and asserts `Error::Corrupt`.
2. `crates/ec-h264/src/decoder.rs`: the chroma reference lookup (`chroma_ref`, inter MB
   path) had `.expect("checked above")`. It now returns `Result` and propagates with `?`,
   the same as the luma lookup next to it (`luma_ref`), which already returned
   `Error::corrupt`. Corrupt `ref_idx` against a shorter list now errors instead of panicking.
3. Clippy fixes in non-input code (no behaviour change):
   - `enc/mb.rs`: `is_multiple_of` for the two `mb_y % 2 == 0` checks; a local `type Best`
     alias; `#[allow(clippy::too_many_arguments)]` with a reason on the three encoder cost
     functions.
   - `inter.rs`: removed a same-type `as i32` cast and redundant parentheses in
     `store_weighted`; `#[allow(clippy::too_many_arguments)]` with a reason on `chroma_rows`.
   - `decoder.rs` test helper `ramp_picture`: struct-literal init instead of field reassign.
   - `tests/encode.rs` (BD-PSNR Gaussian elimination helper): two `needless_range_loop`
     loops rewritten as iterators. Same arithmetic.
4. New `crates/ec-h264/tests/no_panic.rs`: first 10 JVT streams (the name-sorted walk
   `conformance.rs` uses, `take(10)`), 8 truncations at `len*k/9`, and 30 single-byte
   corruptions from a fixed xorshift seed (`0x9E3779B9`), each decoded end to end with all
   errors ignored. A panic fails the test. Skips when `fixtures/` is absent unless
   `EC_REQUIRE_FIXTURES` is set.

## Audit: panic sites in ec-h264/src (decoder side)

`unwrap()` / `expect(` / `panic!` / `unreachable!` outside `#[cfg(test)]`:

| Site | Status | Reason |
|---|---|---|
| `dpb.rs` `store` marking `expect` | fixed | returns `Corrupt` (item 1) |
| `decoder.rs` `chroma_ref` `expect` | fixed | returns `Corrupt` (item 2) |
| `decoder.rs` ~475-476 `sps_map[..].take().expect("checked above")`, `pps_map[..].take()` | safe | the same slots were checked `Some` by `ok_or_else` at ~462-466 in the same call. `sps_id` is `<= 31` (validated in `Pps::parse`, `ec-h264-syntax/src/pps.rs:88`), and `sps_map` has 32 slots. |
| `decoder.rs` ~1178 `intra_type.expect(..)` | safe | `shape` is `None` only on the P (`mb_type >= 5`), B (`>= 23`) or intra branch, which set `intra_type`. The `- 5` / `- 23` subtractions are guarded by those same conditions. |
| `transform.rs` ~167 `try_into().expect` | safe | `d[o..o+8]` with `o = i*8`, `i < 8`, is always 8 long. |

Remaining `unwrap`/`expect`/`panic!` hits are in `enc/` (encoder, not the decoder path) and
in `#[cfg(test)]` modules. The `enc/` ones are not reachable from `push_nal`.

Not done by grep: the unchecked input-derived indexing in `cabac.rs`, `cavlc.rs`,
`inter.rs`, `mv.rs`, `pred.rs`, `entropy.rs`, `bits.rs`. This was not audited line by line.
Coverage for it is the no-panic sweep (item 4) and the existing `corrupt_streams_never_panic`
test (4 streams, CAVLC and CABAC, B slices), both of which pass. The sweep found no further
panic. A line-by-line audit is still open (see below).

## Verify

Run from `~/.cache/wt/h264gates` through `scripts/remote-run.sh h264gates -- ...`.

- `cargo clippy -p ec-h264 --all-targets -- -D warnings`: **pass** (exit 0).
- `env EC_REQUIRE_FIXTURES=1 cargo test -p ec-h264 --release --no-fail-fast`:
  - lib: 81 passed, 0 failed (includes the new dpb regression test).
  - `tests/conformance.rs`: 22 passed, 0 failed, 1 ignored (includes
    `corrupt_streams_never_panic`).
  - `tests/encode.rs`: 14 passed, **1 failed**: `real_library_frames_encode_and_decode_exactly`.
  - `tests/no_panic.rs`: 1 passed (`truncated_and_corrupted_conformance_streams_never_panic`).

The one failure is environmental, not from this lane's changes. The test picks a source
from `fixtures/real-library-manifest.tsv`, and the manifest rows point at
`/home/user/Videos/...`, which do not exist on the server host. With `EC_REQUIRE_FIXTURES=1`
the test asserts before any decoding. The test is never reached by the decode changes. I did
not run it on main to confirm the same result. Without `EC_REQUIRE_FIXTURES` it takes the skip
branch instead.

## Environment notes

- The worktree's `fixtures` symlink points at the local repo's fixtures. `remote-run.sh` rsyncs
  it as a symlink (the `--exclude=/fixtures/` pattern only matches directories), so the host
  got a dangling link and every fixture test failed. Workaround used here: the local
  `fixtures` was replaced with an empty directory and the host link was removed. remote-run then
  links `~/fixtures`. This likely affects other lanes too.
- The first `remote-run` attempt hung in its rsync to host A for over 12 minutes. It was killed,
  and the next run went through.

## Left

- Line-by-line audit of unchecked indexing in `cabac.rs`, `cavlc.rs`, `inter.rs`, `mv.rs`,
  `pred.rs`, `entropy.rs`, `bits.rs`. Only fuzz coverage is in place.
- `real_library` needs a manifest whose paths exist on the server host (or a host-local
  manifest).
- `remote-run.sh` symlink handling (above).
