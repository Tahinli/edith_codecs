# lane/av1oracledepth — depth-generic 4:4:4 ffmpeg oracle, 10-bit depth assert, dead 4:2:2 refusal arm

**Tree.** `lane/av1oracledepth` off main `c38dd788`, worktree
`~/.cache/wt/av1oracledepth`, target dir `~/.cache/tgt-av1oracledepth`.
Scope: `crates/ec-av1/src/stream.rs` ONLY (test-module oracle helpers and
gates). `decode.rs` untouched. No push, no merge.

Closes items 3 and 4 of `lanes/av1oracleguard.report.md` §"Not closed, stated
plainly", plus the dead code arm the lane-av1stale422prose sweep left on
purpose.

## Item 3 — depth-generic 4:4:4 oracle helper

### New helper: `ffmpeg_decode_sequence_444_depth`

Mirrors `ffmpeg_decode_sequence_422_depth` (lane-av1422lift) shape for shape:

* `yuv444p` / `yuv444p10le` / `yuv444p12le`, `depth in {8, 10, 12}` else
  hard assert.
* Container width from `depth`: 1 byte/sample at 8, 2-byte LE at 10/12, on
  BOTH sides, so nothing narrows.
* Shape guard `(sx, sy) == (0, 0)` read from the stream's own sequence
  header (same class as `assert_420_oracle_stream`:
  `helper-assumes-the-shape-the-gate-name-implies`) — a (1,0)/(1,1) stream
  handed here cannot let ffmpeg convert it.
* Depth guard: `stream_bit_depth(stream) == depth` — the wrong-depth refusal
  item 4 asked for, applied here too.
* Frame-count diagnosis reuses `frame_count_diagnosis`.

`ffmpeg_decode_sequence_444` (8-bit) is now a one-line delegate onto it at
`depth=8, "yuv444p"`, so its 8 existing call sites keep their exact contract;
its guard set GROWS (shape + depth now asserted where none was), and all 8
sites still pass.

### New test 1: refusal — `the_8bit_444_oracle_helper_refuses_a_10bit_444_pin`

Pinned real 10-bit 4:4:4 stream `444_lossy_palette_chroma_352x242_10b.obu`
(24 875 B, FNV-1a64 `0x7fda_dbb4_3bbf_9af3`, sha256
`84df2f37cc7982f9a572443d85dfcc13bf366a64f3fb567391dcd5acd58a0ad2`, the same
pin `a_444_lossy_palette_chroma_unit_window_is_byte_exact_at_352x242_10bit`
gates; provenance lane-av1444d10 §0 — census recipe, byte-reproduced). No new
fixture invented. The test asserts the pin is REALLY 10-bit and REALLY (0,0)
before feeding it, then requires `ffmpeg_decode_sequence_444` to panic with a
message naming `not 8-bit` and the sibling
`ffmpeg_decode_sequence_444_depth`. Needs no ffmpeg (guard runs before the
spawn). RED-before is inherent: on the parent helper (no guards) the same
call returned a 4:4:4-sliced converted picture with no refusal — that is the
silent-green the helper now stops.

### New test 2: positive control — `the_444_depth_helper_is_byte_exact_on_a_pinned_10bit_444_cell`

Same pin, `ffmpeg_decode_sequence_444_depth(.., 10, "yuv444p10le")`:

1. 16 shown frames byte-exact vs our decode, per plane, first-diff panic.
2. Flip control (crate's own comparator-liveness shape, thread-local ffmpeg
   shim from `every_ffmpeg_comparator_reds_on_a_one_sample_wrong_ffmpeg`):
   rotating ONE byte of the ORACLE's rawvideo stdout must move the mismatch
   count 0 -> EXACTLY 1 in the tampered plane, 0 in the other two — measured
   for Y, U and V separately. GREEN measured on this host (ffmpeg 8.1.3,
   oracle aomdec present, `EC_AV1_REQUIRE_*` set).

## Item 4 — `ffmpeg_decode_sequence_10bit` depth assert

`assert_eq!(stream_bit_depth(stream, ..), 10, ..)` right after the existing
shape guard, mirroring `assert_12bit_sequence_header`'s role for the 12-bit
gates. The shape refusal is untouched (still `assert_420_oracle_stream`
first, and the wrong-shape message still names both depth-generic siblings).
An 8-bit stream handed to the helper now reds by NAME before ffmpeg runs;
ffmpeg would otherwise convert 8-bit samples into the 16-bit container
silently and the gate could read exact on a converted reference.

New test 3: `the_10bit_oracle_helper_refuses_a_wrong_depth_stream` on the
committed 8-bit 4:2:0 pin `mix176_offtile_chroma_clip.obu` (4 581 B,
FNV-1a64 `0x5e31be5efc9c3c1d`) — the 4:2:0 shape is asserted FIRST so the
red cannot be the shape guard; the message must contain `not 10-bit`.

Sibling strings swept: the stale "ffmpeg_decode_sequence_444 is 8-bit only"
text in the 10-bit and 12-bit shape guards now points at
`ffmpeg_decode_sequence_444_depth(.., N, "yuvNNNle")`.

## Item 3b (bonus sweep): dead refusal arm deleted

`the_pinned_422_palette_intra_in_inter_cell_window_is_byte_exact` — the
prose sweep's leftover. OK arm proved live on the pinned fixture FIRST
(measured: `17 decode-order frame(s) byte-exact against aomdec (1 hidden),
16 chroma palette unit window(s)`), then the `Err` arm deleted. It matched
the pre-lift refusal string `"a chroma format of 4:2:2"` which committed code
no longer returns; its only possible future effect was swallowing a
DIFFERENT decode error whose message happened to contain that string and
reading green off a stream that decoded nothing. The arm is replaced by a
`panic!` on ANY error (4:2:2 is a decoded format; a refusal is a regression).
The stale doc-comment half that described the Err arm is rewritten; the
comment lines the prose merge landed are otherwise untouched. Gate still
green after deletion.

## Runs (all on the lane tree, `EC_AV1_REQUIRE_FFMPEG=1
EC_AV1_REQUIRE_AOMENC=1 EC_AV1_REQUIRE_AOMDEC=1`)

| run | result |
|---|---|
| `cargo check -p ec-av1 --tests` | clean, 0 warnings |
| the 3 new tests + `a_420_oracle_helper_refuses_a_422_pin` + `a_420_high_bit_depth_oracle_helpers_refuse_a_422_pin` | 5/5 green |
| `a_lossless_444_*` + `ll444_*` + superres + palette 4:4:4 family (`444_` filter, 53 tests) | 53/53 green |
| superres family (22 tests, incl. 10/12-bit 444 superres) | 22/22 green |
| `every_ffmpeg_comparator_reds_on_a_one_sample_wrong_ffmpeg` (10bit helper still bites per plane) | green |
| `the_pinned_422_palette_intra_in_inter_cell_window_is_byte_exact` after arm deletion | green (17 frames byte-exact, 16 windowed units) |
| full `-p ec-av1 --lib` suite | see run line below |

## Not closed, stated plainly

* The 8-bit `ffmpeg_decode_sequence_444` delegates through the depth helper,
  so 8-bit 4:4:4 gates now also pay the shape/depth parse (two header
  probes). Measured cost on the 53-test 4:4:4 family: none beyond noise.
* `ffmpeg_decode_sequence_444_depth` is exercised at 8 and 10; the 12-bit
  arm has no committed exactness pin yet (the 12-bit 4:4:4 gates use
  `decode_all_frames_vs_oracle`, not ffmpeg rawvideo). The helper's 12-bit
  container path is the same `bps == 2` branch the 10-bit pin proves.
