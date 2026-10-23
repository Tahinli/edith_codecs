# Lane av1conf: libaom AV1 conformance (profile 0, 8 and 10 bit)

## Goal

DONE.md, ec-av1 decode (P0): "The AOM conformance streams for profile 0, 8 and 10 bit, are
bit-exact vs libaom/dav1d." This lane fetches the libaom test vectors, adds an integration test that
compares every shown frame with the libaom `.md5` file, runs it on a server and reports the result.

Base: `main` at `f5eb855b` (lane `lane/av1conf` created from it).

## Result

**240/242 exact. 0 refused by name. 2 known failing (spatial layers). 0 unexplained failures.**

DONE.md criterion "AOM conformance streams for profile 0, 8 and 10 bit are bit-exact": **NOT MET**.
The 240 vectors that cover the profile 0 quantizer, size, intra, mv, mfmv, cdf update, film grain,
monochrome and allintra streams match. The two SVC L2 spatial-layer vectors do not (follow-up items 1
and 2 below).

## The vector set

The list is libaom's `test/test_vectors.cc` on `aomedia.googlesource.com/aom/+/refs/heads/main`
(`?format=TEXT`, base64-decoded). It has 242 unique `av1-1-b*` names. All 242 have a `.md5` in the
bucket.

- Bucket: `https://storage.googleapis.com/aom-test-data/<name>` (and `<name>.md5`).
- Profile 0, 8 bit (`av1-1-b8-*`, 176 vectors): `00-quantizer-00..63` (64), `01-size-WxH` for
  W,H in {16,18,32,34,64,66} (36) and {196,198,200,202,208,210,224,226} (64), `02-allintra.ivf`,
  `03-sizedown.mkv`, `03-sizeup.mkv`, `04-cdfupdate.ivf`, `05-mv.ivf`, `06-mfmv.ivf`,
  `16-intra_only-intrabc-extreme-dv.ivf`, `22-svc-L1T2.ivf`, `22-svc-L2T1-2.ivf`,
  `22-svc-L2T2-2.ivf`, `23-film_grain-50.ivf`, `24-monochrome.ivf`.
- Profile 0, 10 bit (`av1-1-b10-*`, 66 vectors): `00-quantizer-00..63` (64),
  `23-film_grain-50.ivf`, `24-monochrome.ivf`.
- Not in the set: any 4:4:4, 4:2:2, 12-bit, large-scale-tile or annexb vector. The bucket also has
  `.orig` copies of two SVC files; they are not in `test_vectors.cc` and were not fetched.

Fixtures: `~/fixtures/av1-aom/` on hosts a (`user@host-a`), b (`user@host-b`)
and c (`user@host-c`). Each has 484 files (242 vectors and 242 md5), 7.5 MB, and the same
checksum (sha256 of `sha256sum *`, first 16 hex: `1b54d879720d33dd`). The name list is
`~/fixtures/av1-aom.vectors.txt` on each host. The fetch script was a plain `curl -fsS` loop.
Nothing is written under the repo.

## Test

`crates/ec-av1/tests/aom_conformance.rs` (new, about 450 lines).

- Reads `EC_AV1_AOM_DIR`. Unset or unreadable: prints `SKIP` and returns. With
  `EC_REQUIRE_FIXTURES=1`, a missing dir fails the test.
- IVF (32-byte header, 12-byte frame headers) and Matroska (flat EBML walk, as in
  `ec-vp9/tests/libvpx_vectors.rs`) demux. The IVF/MKV payloads are concatenated and passed to
  `ec_av1::stream::decode_stream_with`, which is the decoder's raw OBU entry point.
- The per-frame digest is libaom `test/md5_helper.h`: MD5 over the cropped planes Y, U, V (chroma
  `(w+1)/2 x (h+1)/2`), each sample 1 byte at 8 bit, 2 bytes little-endian above 8 bit.
  Monochrome: libaom digests Y, then two 4:2:0 chroma planes of mid-grey `1 << (bd-1)`. This was
  checked: ffmpeg's `gray` frame 0 of `av1-1-b8-24-monochrome.ivf` gives the Y part; the libaom
  line `605ae8d3...` is reproduced only with those two grey planes appended.
- Each shown picture is compared with the next line of the `.md5`. The first mismatch is reported
  with the vector name, the frame index and the frame size. A frame-count difference is also a
  failure.
- Refusals by name, checked before decode: `seq_profile != 0`, bit depth above 10, and non-4:2:0
  chroma when not monochrome. A decoder `Error::Unsupported` also counts as refused by name. Other
  decoder errors are failures.
- `KNOWN_FAILING` (same pattern as `ec-vp9/tests/libvpx_vectors.rs`): the test passes when exactly
  the listed vectors diverge. A listed vector that starts matching fails the run until it is removed.

## Verify commands

All on host a (`scripts/remote-run.sh av1conf --host a`), `CARGO_TARGET_DIR=~/.cache/tgt-av1conf`:

1. `EC_AV1_AOM_DIR=$HOME/fixtures/av1-aom EC_REQUIRE_FIXTURES=1 EC_MEMGUARD_MAX=6G cargo test --release -p ec-av1 --test aom_conformance -- --nocapture --test-threads=1`
   Result: `aom vectors: 240/242 exact (595 shown frames), 0 refused by name, 2 known failing, 0 failed`.
   `test result: ok`.
2. `cargo clippy -p ec-av1 --all-targets -- -D warnings`: exit 0.
3. `EC_MEMGUARD_MAX=6G cargo test --release -p ec-av1 -- --test-threads=4`: passes (below).

Suite result: exit 0. `test result: ok. 811 passed; 0 failed; 49 ignored` for the main unit set (3253 s), plus the
smaller test binaries, all `ok`. The aom test skips in this run because `EC_AV1_AOM_DIR` is unset.

## Failures and follow-up items

Both failures are the same vector family, the L2 SVC streams with two spatial layers. They are
`av1-1-b8-22-svc-L2T1-2.ivf` and `av1-1-b8-22-svc-L2T2-2.ivf`. Each has a `.md5` whose lines name
`1280x720`. Both fail the same way: the first shown frame (`0`) does not match, and the decode then
stops after one frame.

Observed in the run:

```
av1-1-b8-22-svc-L2T1-2.ivf: first mismatch at frame 0 (640x360): md5 f3874ea6... != d059f1db...
av1-1-b8-22-svc-L2T1-2.ivf: refused after 1 frames: AV1 tile: a reference picture whose height does not match this frame's own true size
```

(`L2T2-2` gives the same two lines.)

1. **Spatial layer output policy.** libaom's default (`output_all_layers = 0`) outputs one frame per
   temporal unit, the top spatial layer. The md5 lines (1280x720) show that. This decoder's
   `decode_stream_with` sink receives every shown frame, so the first output is the 640x360 base
   layer. Fix: the output rule belongs in the decoder or its caller. It needs temporal-unit
   boundaries, which `decode_stream_with` does not expose. Not a small fix.
2. **Scaled reference prediction.** The 1280x720 layer predicts from a 640x360 reference, and the
   decoder refuses it with `AV1 tile: a reference picture whose height does not match this frame's
   own true size`. The decoder does not implement scaled-reference motion compensation (spec 7.11.3.3
   with a different reference size). This is a decoder feature, not a small obvious fix. It is
   reported as its own root cause, not as a symptom of item 1.

The mismatch list above is the whole set of failing vectors. No other vector mismatched.

## Not covered

- **Refused by name: 0.** No vector in the libaom set uses 4:4:4, 4:2:2, 12-bit, large-scale tile or
  annexb, so nothing exercised the refusal branches. They are in the test code but not run against a
  real stream. A profile 1 or 2 stream would be needed for that check.
- The Argon set is not run (not required).
- No decoder code changed. The only changed file is the new test. A fix was not attempted for either
  follow-up item.
- Host b and c runs were not repeated after the fixture download; the checksums match host a, and
  the run was on host a only.
