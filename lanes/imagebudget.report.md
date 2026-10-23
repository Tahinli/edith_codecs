# lane imagebudget: ec-image default decode budget (512 MiB), replay refusals, decode fuzz gate 2

Base `b597ac00` (lane/fuzz1hcontainers). Branch `lane/imagebudget`.

## Goal

Policy (orchestrator): `ec-image` gets a default decode budget equal to the `image` crate's default,
`max_alloc` = 512 MiB, for the decoded output. `decode` enforces it; `decode_with_limits` lets a caller
raise or remove it. A picture whose width x height x bytes-per-pixel exceeds it is refused before any
allocation or pixel work, with an error naming the dimensions and the budget. Existing width and height
limits are unchanged (`max_pixels` stays 2^28). Gate 2 for `ec-image decode` needs a clean 1 h fuzz run.

## Changes

1. `src/lib.rs`: `DEFAULT_MAX_ALLOC = 512 << 20`; `Limits::default().max_alloc` is now that value (was 1 GiB).
   New `Limits::check_output(width, height, bytes_per_pixel)`: runs the existing `check`, then refuses
   `w*h*bpp > max_alloc` with `Error::unsupported("WxH image", "N bytes of decoded pixels past the
   536870912 byte budget (512 MiB by default; decode_with_limits raises it)")`.
2. Output-sizing sites now call `check_output` with the bytes per pixel of the form the decoder emits:
   - BMP: 3 or 4 (alpha) per pixel (`bmp.rs`).
   - PNG: channels x (2 for 16-bit, else 1); the existing 16-bit expansion check stays after it (`png.rs`).
   - JPEG: 1 for grey, 3 for colour, before the planes are formed (`jpeg.rs`, `reconstruct`).
   - GIF: RGBA canvas, 4 (`gif.rs`; the old canvas-bytes check was replaced).
   - WebP lossy: 4 with alpha, else 3, checked before the VP8 planes are decoded (`webp/mod.rs`).
   - WebP VP8L: 4 (`webp/vp8l.rs`). WebP animation canvas: 4 (`webp/mod.rs`).
   - TIFF: samples (3 for a palette file) x (2 for 16-bit, else 1) (`tiff.rs`).
3. Replay test `tests/fuzz_regressions.rs::large_declared_bmp_pictures_are_refused_before_decoding`
   (replaces the old "returns" probe): the three RLE8 BMP replay inputs must return `Err` naming
   `536870912`, and in release each must return in under 50 ms (asserted, passes on host b).
   Note: only two RLE8 BMPs were committed. The third, 64x3014704, was added from host c's fuzz output
   (`tests/data/fuzz/decode-bmp-rle8-64x3014704-timeout.bmp`, 4,948 bytes).
   The JPEG replay (65519x256, about 50 MB of RGB) is under the budget, so it still decodes; the test
   only asserts that it returns.
4. Fuzz findings from this lane's own runs (all fixed, each with a replay test):
   - `decode-jpeg-gray-4353x20297-slow-unit.jpg` (run 1, slow unit; run 2 log): a grey JPEG whose file ends
     after the frame header. It used to build an 88 Mpx grey picture of blocks with no data. A frame with no
     scan is now refused (`Corrupt`, "a frame header with no scan"). Test
     `a_frame_with_no_scan_is_refused_without_building_the_picture`.
   - `decode-jpeg-3comp-167mpx-timeout.jpg` (run 2 timeout, 65535x2559, 3 components, no scan): refused by the
     same rule. Test `a_large_three_component_frame_with_no_scan_is_refused`.
   - Scan that runs out of data: `BitReader` counts padding bytes loaded per restart interval; the MCU loop
     stops once 8 have been loaded (the fill runs at most 4 bytes ahead on a valid file), as libjpeg does.
     The blocks not reached stay zero. This changes the output of truncated JPEG scans (remainder grey instead
     of decoded noise). Test `a_scan_that_runs_out_of_data_stops_and_returns`.
   - Speed, output identical (checked by the existing differential suites and the new unit test
     `the_dc_only_shortcut_matches_the_full_transform`): DC-only IDCT shortcut; crop copies rows
     (was a per-element `flat_map`); colour conversion writes in place with `as_chunks_mut`; upsample builds
     the column index table once and skips the vertical copy when the height is unchanged.
5. Unit test `check_output_names_the_picture_and_refuses_past_the_budget` (lib.rs).
6. BMP refusal test expectation moved from "BMP pixels" to "1000x1000 image" (the same refusal, now named).

## Edith (read-only check)

- `/home/user/Documents/Code/Rust/edith`: `crates/engine` and `crates/app` depend on the crates.io `image`
  0.25 crate (`crates/engine/Cargo.toml` lines 25 and 216; `crates/app/Cargo.toml` line 13). There is no
  `[patch]` for `image`, and nothing in edith references `ec_image` or `shims/image`. So edith is not
  affected by this change today.
- The image shim (`shims/image/src/lib.rs`, `gpui.rs`) decodes through `ec_image::decode` and
  `ec_image::decode_animation` with the default `Limits`, so after this change its default is 512 MiB, the
  same as the `image` crate's default. No shim change was needed, so no shim gate was run.
- Not verified: whether upstream `image` 0.25 applies its 512 MiB limit on edith's `into_decoder` +
  `from_decoder` path (`crates/engine/src/decode.rs` ~1826-1832). That is for the swap lane.

## Verify (run through scripts/remote-run.sh, host b, final code)

| command | result |
|---|---|
| `EC_REQUIRE_FIXTURES=1 scripts/remote-run.sh imagebudget --host b -- cargo test --release -p ec-image` | EXIT 0; result blocks 37, 19, 19, 10, 10, 2 passed, 0 failed |
| `... -- cargo clippy -p ec-image --all-targets -- -D warnings` | EXIT 0, no warnings |
| `... -- cargo test -p ec-image --test fuzz_regressions` (debug, overflow checks) | 10 passed, EXIT 0 |
| `... -- cargo test -p ec-image --lib` (debug) | 37 passed, EXIT 0 |

The release replay test passes the 50 ms bound for all three BMP inputs. The elapsed times were not printed.

## Fuzz (gate 2): `cargo +nightly fuzz run decode -- -max_total_time=3600 -rss_limit_mb=2048 -timeout=10`

| run | host | runs | exec/s | result | code |
|---|---|---|---|---|---|
| run 1 | b | about 4.4 M in about 25 min when stopped | about 30k at start, about 4k later | stopped by me after it wrote `slow-unit` for the 88 Mpx grey no-scan frame (no crash). Stale code, not counted | budget + IDCT shortcut, before the no-scan refusal |
| run 2 | b | about 2.4 M in about 33 min (count at exit not kept) | 27k falling to 1.3k | exit 70: `timeout` after 12 s, `decode-jpeg-3comp-167mpx-timeout.jpg` (65535x2559, no scan) | before no-scan refusal |
| **run 3 (final)** | **b** | **4,832,220** | **1,342 avg** | **clean: `Done 4832220 runs in 3601 second(s)`, FUZZ_EXIT=0, no artifacts, no crash, OOM or timeout** | **this commit** |

Run 3 started with a fresh corpus: an accidental `remote-run -- true` sync (rsync --delete) removed the corpus
kept from run 2. Host b's slot 2 was used for all three runs.

## Left / caveats

- Performance margin: a legal 167 Mpx, three-component JPEG with real scan data (under the 512 MiB budget)
  takes about 3.8 s native and about 10 s in the ASan fuzz build (measured before the no-scan refusal, on
  the scan path). That is at the fuzz timeout under load. The fix is a fused upsample and colour pipeline,
  which this lane did not do. Run 3 did not hit it, but it could.
- The scan-runs-out stop changes truncated-JPEG output (see change 4). Downstream users that compare truncated
  JPEG pixels will see grey instead of noise.
- Host c's disk was at 100% (other lanes' target directories: edith-integ 25 GB, edithgaps1 13 GB). I removed
  only `~/.cache/tgt-imagebudget` (857 MB) there. Host c still has about 0.9 GB free. Nothing else was deleted.
- `shims/image` was not changed and not gated.
