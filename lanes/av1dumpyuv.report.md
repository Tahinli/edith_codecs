# lane-av1dumpyuv — the depth-packing class outside the test helpers

Branch `lane-av1dumpyuv`, branched from `afa13bcf` ("Merge lane-whtshape").
Worktree `~/.cache/wt/av1dumpyuv`. Examples/tools/scripts only: no decoder
logic, no gate touched. The primary checkout was never edited
(`git status --porcelain` there is empty).

## 1. The class

`aomdec --rawvideo` and `ffmpeg -f rawvideo` pack **8 bits per sample for an
8-bit stream** and **16 bits per sample (`*p10le` / `*p12le`) for a
high-bit-depth one** — they never pack to the stream's own bit depth. A site
that reads decoded `u16` planes and writes them `as u8`, or writes them `u16`
unconditionally, therefore produces a file of the wrong size, and the next
person's `cmp` against the oracle fails on LENGTH and reads like a decoder
bug. The prior audit (lanes/av1cmpaudit.report.md) fixed the test helper
`assert_rawvideo_matches`'s sibling `EC_AV1_FINAL_DUMP`
(`crates/ec-av1/src/stream.rs:2137-2152`, depth-correct since lane-hidden r1)
and named exactly one unfixed non-test site: `examples/dump_yuv.rs`.

## 2. Sweep of the non-test surfaces

Rows verified by reading each file. "Assumes" = what the code takes the
bits-per-sample to be.

| site | assumes about depth | derived from | failure mode on the wrong depth |
|---|---|---|---|
| `crates/ec-av1/examples/dump_yuv.rs:33` (pre-fix) | always 2 bytes/sample (u16 LE) | **nothing** | silent 2x file on an 8-bit stream; docstring named `yuv420p10le` as the companion, so a reader hands it 8-bit input and diffs the size |
| `crates/ec-av1/examples/dump_yuv.rs` (**fixed**) | sequence header `color_config.bit_depth` | the stream's own parsed header; `--depth` is an ASSERTED argument | hard exit 1 naming both depths, no file |
| `crates/ec-av1/examples/decode_probe.rs:434-462` | `EC_PROBE_OUT16` = 2 B/sample, `EC_PROBE_OUT`/argv[2] = 1 B/sample | two SEPARATE, explicitly named output targets, each a fixed choice by the caller | none for a single file: the 16-bit target is a full 16-bit container (same as `-pix_fmt yuv420p10le` on an 8-bit stream), the 8-bit target narrows. Both are written, so a wrong choice is visible as two files of different sizes, not one mislabelled file. **not fixed** |
| `crates/ec-vp8/examples/dump_frame.rs:28-45` | 1 byte/sample | VP8 has no high-bit-depth profile (its 8-bit 4:2:0 is the format) | none possible. **not flagged** |
| `crates/ec-av1/examples/{alloc_probe,enc_probe,syntax_census}.rs` | — | no plane is read or serialised | none. **not flagged** |
| `crates/ec-h264/examples/bench_decode.rs`, `bench_encode.rs`, `crates/ec-aac/examples/*`, `crates/ec-ac3/examples/ac3dec.rs` | — | audio `f32`/metric only; no video planes | none. **not flagged** |
| `scripts/instrument-aom-oracle.sh:130-145` (rung PREFILT), `:333-350` (POSTDEBLOCK), `:384-398` (PREFILT_WIDE), `:880-896` (POSTCDEF) | 1 byte/sample, no `YV12_FLAG_HIGHBITDEPTH` check | nothing | on a 10/12-bit stream: a **half-length** file (u8 rows of a u16 plane) that can still byte-match a decoder-side `as u8` narrowing. Rung 12 (`FINAL`, `:684-705`) IS depth-correct and is what `EC_AV1_FINAL_DUMP` pairs with. **NOT FIXED** — these are oracle C rungs whose decoder-side counterparts (`EC_AV1_PREFILT_DUMP`, `EC_AV1_DECODE_ORDER_DUMP`, and the `as u8` debug dumps at `stream.rs:2123-2126`) narrow identically on purpose; changing one side alone would break the byte-pairing those rungs exist for, and both sides are decoder/oracle logic, out of this lane's scope. The script's own comment at `:659-660` already calls them the "u8-narrowing debug dumps". |
| `scripts/lr-sgr-pin-harness.c:33-36` (pre-fix) | 1 byte/sample of `/tmp/lr_full.bin`; `bit_depth=8, highbd=0` hardcoded at `:47`, `:54`, `:75` | nothing (the capture is a bare `std::fs::write` from a since-reverted `EC_LR_DUMP` in `apply_sgrproj_stripe`, lanes/lr.report.md:352-354) | a 10-bit re-capture is exactly 2x the bytes; the harness read its first half as plane data and printed plausible A/B taps for the wrong pixels | 
| `scripts/lr-sgr-pin-harness.c` (**fixed**) | still reads 8-bit, but the file length is now an ASSERTION | file size, checked against `bw*bh` | 2x capture → exit 1 naming the byte counts, the assumed depth, and the uint16 port; truncated → exit 1 |
| `scripts/raw_to_y4m.py:9` (pre-fix) | `w*h*3//2` = 8-bit 4:2:0, unconditionally; header hardcoded `C420jpeg` | nothing | a 10-bit stream was cut at half frame length, frame N+1's chroma read as frame N's tail, trailing partial frame **dropped silently** — and vpxenc encodes the result without complaint |
| `scripts/raw_to_y4m.py` (**fixed**) | required `DEPTH` argument, asserted against the stdin length | explicit asserted argument (no parser exists for a headerless raw stream) | non-integer frame multiple → exit 1 naming both implied frame sizes; a missing/!8/10/12 depth → usage error |
| `scripts/superres-pin-harness.c:14-17,50,80,89-142` | — | coverage gap, not depth: only the `uint8` `av1_convolve_horiz_rs_c` arm is called and the input rows were transcribed from an 8-bit fixture | a 10-bit superres pin would be silently uncovered. Different class; **not fixed** |
| `tools/oracle/src/main.rs:352-357` (`sample`), `:383-390`, `:414`, `:424-425`, `:479` | bytes-per-sample from the `--pix-fmt` string (or ffprobe of the reference CONTAINER, `:383-388`), never from a parsed header | explicit CLI flag / container probe | a wrong `--pix-fmt` yields `fa != fb`, and `len_ok = fa == fb` feeds the verdict at `:479` → **loud FAIL**, not a silent compare. Acceptable shape (an argument that is checked). **not fixed** |
| `tools/ec-bench/src/main.rs:253`, `:367`, `:557` | 1 byte/sample `frame_len`; `:564-571` widens those 8-bit samples into `Av1Picture`'s `u16` planes | the same forced `-pix_fmt yuv420p` on the extracting ffmpeg (`:242-243`) | self-consistent: the frame arithmetic and the file it slices come from one explicit ffmpeg argument. Mild real hole: `Av1Config` (`:536-544`) has no depth field, so the encoder's own output depth is whatever it defaults to. **not fixed** (bench table, no oracle compare) |
| `tools/ec-bench/src/main.rs:404-410`, `:423`, `:452`, `:485` | 10-bit for the `ec-hw` row | a comment about the local VA driver's stored format, plus a matching `-pix_fmt yuv420p10le` at `:485` | arithmetic agrees with the forced format. **not fixed** |
| `shims/**` | — | 16 Cargo path-shim crates (`Cargo.toml` + thin `lib.rs` re-exports), no video or raw-byte code | none. **out of scope** |
| `scripts/{build-aom-oracle,build-aom-affine-oracle,gen-bitstream-fixtures,gen_vp8_fixtures,gen-still-fixtures,pgs-white-level,scan-real-library,fetch-vectors,link-fixtures}.sh`, `scripts/av1-timeline-report.py` | force an explicit `-pix_fmt` (or read no planes at all); sizes that exist (`gen_vp8_fixtures.sh:60`, `pgs-white-level.sh:27`) are computed from that same argument | CLI flag | none. **not flagged** |

Sites fixed: 3 (`examples/dump_yuv.rs`, `scripts/raw_to_y4m.py`,
`scripts/lr-sgr-pin-harness.c`). Sites examined and deliberately NOT fixed:
7 (`decode_probe.rs`, `tools/oracle/src/main.rs`, `tools/ec-bench` x2,
`instrument-aom-oracle.sh` rungs 1/6/7/15, `superres-pin-harness.c`), each with
the reason above.

## 3. `examples/dump_yuv.rs` — what changed

- The depth comes from the stream's **own parsed sequence header**
  (`ec_av1_syntax::Av1Parser` walking OBU by OBU until a sequence header
  appears, then `seq.color_config.bit_depth`). It is never taken from the file
  name, the extension, or a reference tool's `-pix_fmt`.
- 8 bit → one byte per sample; 10/12 → little-endian `u16` (the container
  both oracles use). Monochrome streams write the luma plane only.
- `--depth N` is an **assertion, not a switch**: the packing always follows the
  parsed header, and a contradiction exits 1 naming both depths, both
  bytes-per-sample values, and the size mismatch the `cmp` would have shown.
  Two spellings accepted (`--depth 8`, `--depth=8`); given twice → usage error.
- A stream with no parseable sequence header → hard error instead of a guess.
- A sample that does not fit the container about to be written (an 8-bit
  header with a sample above 255) → hard error. That is the mirror of the
  original bug and the reason `as u8` may never be applied silently here.
- The docstring no longer says `yuv420p10le`; it states the both-depths
  contract and the `--depth` assertion.
- Measured while doing this: the crate's own 8-bit fixture
  `av1_192x128_8bit_intra64_in_inter.obu` parses `subsampling_x/y = 1/1`,
  which is **4:2:0** (spec 5.5.3) — 36864 samples for 192x128 = W*H*3/2. The
  first pix_fmt table I wrote had 1/1 → 4:4:0 and the measurement caught it;
  the shipped table is 0/0→444, 0/1→440, 1/0→422, 1/1→420.

## 4. Proof

Committed crate fixtures only, worktree at `afa13bcf`,
`CARGO_TARGET_DIR=$HOME/.cache/cargo-target`.

### 4.1 GREEN — 8-bit stream, committed fixture

```
$ dump_yuv crates/ec-av1/fixtures/av1_192x128_8bit_intra64_in_inter.obu /tmp/dy/eight
sequence header: bit_depth 8 -> 1 byte(s) per sample, seq_profile 0, subsampling 1/1, mono_chrome false; ffmpeg -pix_fmt yuv420p
frame 0: 192x128 -> /tmp/dy/eight.f0.yuv (36864 samples, 36864 bytes, yuv420p)
...
eight.f0.yuv 36864 bytes          <- W*H*3/2 = 192*128*3/2 = 36864  (Y 24576 + U 6144 + V 6144)
```
Byte-exact against the oracle at its own depth:
`aomdec --codec=av1 --rawvideo -o ref8.raw <fixture>` → 221184 bytes for 6
frames; our 6 concatenated frames → 221184 bytes; `cmp` → **IDENTICAL**.

### 4.2 GREEN — high-bit-depth stream, committed fixture

```
$ dump_yuv crates/ec-av1/fixtures/av112bit-key.obu /tmp/dy/twelve
sequence header: bit_depth 12 -> 2 byte(s) per sample, seq_profile 2, subsampling 1/1, mono_chrome false; ffmpeg -pix_fmt yuv420p12le
frame 0: 160x128 -> /tmp/dy/twelve.f0.yuv (30720 samples, 61440 bytes, yuv420p12le)
twelve.f0.yuv 61440 bytes         <- W*H*3 = 160*128*3 = 61440  (30720 samples x 2 bytes)
cmp twelve.f0.yuv (ffmpeg -f obu -pix_fmt yuv420p12le -f rawvideo) -> IDENTICAL
```

### 4.3 The wrong depth fails loudly and writes nothing

```
$ dump_yuv .../av1_192x128_8bit_intra64_in_inter.obu /tmp/dy/wrong --depth 10
dump_yuv: ...: --depth 10 contradicts the stream's own sequence header, which carries
bit_depth 8 (seq_profile 0, subsampling 1/1, mono_chrome false). Writing 10-bit would emit
2 bytes per sample against the oracle's aomdec --rawvideo / ffmpeg -pix_fmt yuv420p (1 bytes
per sample, W*H*3/2 for 4:2:0), so every `cmp` would fail on size alone and read like a decoder
defect. Nothing was written.
exit=1
ls: cannot access '/tmp/dy/wrong.f0.yuv': No such file or directory

$ dump_yuv .../av112bit-key.obu /tmp/dy/wrong2 --depth=8      -> same message, exit=1, no file
$ dump_yuv .../av1_192x128_... --depth=8  -> runs, 36864 bytes   (assertion holds)
$ dump_yuv .../av112bit-key.obu    --depth 12 -> runs, 61440 bytes
```

### 4.4 RED-BEFORE — the depth derivation reverted

`git show HEAD:crates/ec-av1/examples/dump_yuv.rs` restored, rebuilt, same
8-bit fixture, same command:

```
--- RED (pre-fix, u16 unconditional) on the 8-bit fixture ---
ours  73728 bytes                      <- 2 x 36864
aomdec --rawvideo 221184 bytes         <- 6 x 36864
cmp exit=1 (mismatch on size alone)
```
73728 = 2 x (W*H*3/2): exactly the silent 2x file the lane was chartered to
close. Fixed tree restored and rebuilt afterwards.

### 4.5 `scripts/raw_to_y4m.py`

```
$ ... | raw_to_y4m.py 64 64 25 g8.y4m 8    -> YUV4MPEG2 ... C420jpeg,  18491 bytes (3 frames)
$ ... | raw_to_y4m.py 64 64 25 g10.y4m 10  -> YUV4MPEG2 ... C420p10,   36922 bytes (3 frames)
$ 12289 bytes | raw_to_y4m.py ... 8
raw_to_y4m.py: stdin is 12289 bytes, not a whole number of 64x64 4:2:0 frames at 8-bit
(1 bytes per sample, 6144 bytes each): 1 trailing bytes. At the other depth a frame would be
12288 bytes and each frame would carry half a frame of the next one's planes. Refusing to
write a misframed y4m.        exit=1, no file
$ DEPTH 7     -> "DEPTH 7 is not 8, 10 or 12 ..."  exit=1
$ DEPTH omitted (the old 4-arg invocation) -> usage error, exit=1
```
Documented limit, stated in the script's own docstring: raw planar video
carries no depth, so a 10-bit stream of N frames is exactly 2N 8-bit frames
and no length test can separate them. That case is caught by the caller
asserting the right DEPTH — which is why the argument is required, not
defaulted.

### 4.6 `scripts/lr-sgr-pin-harness.c`

Linked against `~/.cache/aom-oracle/build/libaom.a`, `/tmp/lr_full.bin`
faked at three sizes:

```
1020 bytes (correct)  -> ret=0 / out[1][6] = 151 / dgd[1][6] = 151     (real libaom taps printed)
2040 bytes (2x, HBD)  -> "/tmp/lr_full.bin is 2040 bytes, this harness reads it as 8-bit
                          (1020 bytes: 102x10 at 1 byte per sample). That is exactly a 2x-size
                          high-bit-depth capture: re-capture it as 8-bit, or port the harness to
                          the uint16 arm (bit_depth=10, highbd=1) before trusting a single
                          printed tap."   exit=1
 900 bytes (short)    -> "... 900 bytes ... Recapture it, or fix bw/bh to the plane the
                          capture came from."   exit=1
```

## 5. Not done, deliberately

- `instrument-aom-oracle.sh` rungs 1/6/7/15 (section 2): changing one side of
  a byte-paired rung breaks the pairing, and both sides are decoder/oracle C.
  Rung 12 is already depth-correct; that is the rung the depth-sensitive
  compares use.
- `tools/oracle/src/main.rs` and `tools/ec-bench/src/main.rs`: already the
  "explicit argument that is checked" shape (a length/frame-count FAIL), with
  no header parser reachable from those binaries to derive a better answer
  from (`oracle` drives ffmpeg through std pipes and is deliberately
  dependency-free).
- `decode_probe.rs`: its two output targets are separate, explicitly named
  files; see section 2.
- No gate, counter, decoder path or fixture was touched.
