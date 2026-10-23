# lane mpeg2crash: ec-mpeg2 panic on a sequence extension that changes MB rows

Base: `main` (`ff89059b`), branch `lane/mpeg2crash`. Not merged.

## Root cause

The fuzz crash input (7657 bytes) has a sequence header for 176x144 (MPEG-1 geometry, 9
macroblock rows), then a sequence extension (0xB5, id 1) that switches the stream to
MPEG-2 interlaced geometry (10 macroblock rows) with the same width, height and chroma
format. The extension's "changed" check compares only width, height and chroma, so the
picture pool is not cleared. Frames allocated earlier keep 144-line planes, but the frame
decode loops over 10 rows, and the row-9 write runs 16 bytes past the plane
(`range end index 25360 out of range for slice of length 25344` at `decoder.rs:1106`).

## Fix

- `crates/ec-mpeg2/src/decoder.rs`
  - `alloc_pic`: a reused pool slot whose stride or height no longer matches the current
    geometry gets fresh planes. Stale planes are never reused.
  - `slice_inner`: a slice whose target picture height is not `seq.mb_height() * 16` returns
    the existing error path, so the slice is concealed and the frame is marked corrupt by
    `end_picture`.
- Clearing the pool in the extension path was rejected: a sequence header that resets the
  geometry is followed by its extension in valid streams, so clearing there would drop
  references mid-GOP.

## Test

- `crates/ec-mpeg2/tests/robust.rs`: `mb_height_change_without_resize_is_concealed`
  decodes `crates/ec-mpeg2/tests/data/fuzz/decode_mpeg2-mb-height-change.m2v` (the crash
  input) and asserts that a send/flush error or a corrupt picture comes back, with no panic.
- Pre-fix on a server the test panicked at `decoder.rs:1106:26` with the reported message.

## Verification (server, via `scripts/remote-run.sh mpeg2crash`)

- `EC_REQUIRE_FIXTURES=1 cargo test --release -p ec-mpeg2 -p ec-mpegts`: every test binary
  ok, EXIT=0 (byte-exact parity tests included).
- `cargo clippy -p ec-mpeg2 -- -D warnings`: EXIT=0, no warnings.

## Fuzz

- Target `decode_mpeg2`, host b, after the fix.
- Command: `cargo +nightly fuzz run decode_mpeg2 -- -max_total_time=3600 -max_len=8192
  -rss_limit_mb=1536 -timeout=30`.
- Seeds: the 893 saved corpus files plus the crash input (894 files).
- Result: `Done 430880 runs in 3603 second(s)`, cov 1807, ft 9673, EXIT=0, no crashes.
