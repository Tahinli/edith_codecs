# lane-av1rawhelper — `assert_rawvideo_matches` cannot judge an 8-bit stream

**Branch:** `lane-av1rawhelper`
**Base:** `7f8817cbcbb567fd1c037c2d3780bdf12c0d2b14` ("Merge lane-av1pins @ 44a05b40 — three
recovered pins committed under the crate + gate rewiring"), taken at lane start while the
13-branch merge wave was landing in main. Worktree `~/.cache/wt/av1rawhelper`; main was never
touched.
**Scope:** tests + test helpers only. No decoder logic touched (`git diff --stat`:
`crates/ec-av1/src/stream.rs` only, and only its `#[cfg(test)]` module).

## 1. Audit

### 1a. `assert_rawvideo_matches` call sites

Before this lane the helper had exactly **one** call site in the whole crate. It packed our
samples as `u16` unconditionally (`for s in p { ours.extend_from_slice(&s.to_le_bytes()) }`)
while `aomdec --rawvideo` writes 8 bits/sample for an 8-bit stream.

| file:line | gate | stream bit depth (source) | helper packing matches? | outcome on a real run |
|---|---|---|---|---|
| `stream.rs:6877` (pre-fix) | `a_lossless_444_128_root_lossless_stream_reads_chunks_chunk_major` control arm | **10-bit**, from the gate's own recipe doc (`testsrc2 128x96 yuv444p10le`, `--profile=1 --lossless=1`) and from the fixture's parsed sequence header | YES — `u16` LE is what `--rawvideo` writes at 10-bit | PASS, a real compare: 6 frames byte-identical. Re-verified post-fix (see §3) |
| *(none — the defect was latent)* | any 8-bit gate | 8-bit | NO — `u16` pack vs 8-bit oracle | dies on `raw sizes differ` with no sample compared |

There was no in-tree 8-bit caller. That is exactly why the defect survived: the single caller
happened to be high-bit-depth. The handing lane hit it the moment an 8-bit gate appeared.

### 1b. Every other raw-comparison helper in the crate

| helper | file:line | oracle | packing | 8-bit safe? | verdict |
|---|---|---|---|---|---|
| `decode_all_frames_vs_oracle` | `stream.rs:9217` | `EC_AV1_FINAL_DUMP` rung 12, both sides | **depth-aware on BOTH sides** — ours at `stream.rs:2142` (`if bit_depth == 8 { as u8 } else { to_le_bytes() }`), the oracle at `scripts/instrument-aom-oracle.sh` rung 12 (`YV12_FLAG_HIGHBITDEPTH` → `fwrite(...,2,...)` else `(...,1,...)`) | YES | correct as written; the depth is the frame's own `bit_depth`, not the gate's label |
| `ffmpeg_decode_sequence` | `stream.rs:5270` | `ffmpeg -pix_fmt yuv420p` | 8-bit by construction, size-asserted before unpacking | n/a (8-bit only) | correct |
| `ffmpeg_decode_gray_sequence` | `stream.rs:5322` | `ffmpeg -pix_fmt gray` | 8-bit, mono | n/a | correct |
| `ffmpeg_decode_sequence_10bit` | `stream.rs:5353` | `-pix_fmt yuv420p10le` | 2-byte LE, size-asserted | n/a | correct |
| `ffmpeg_decode_sequence_12bit` | `stream.rs:13211` | `-pix_fmt yuv420p12le` | 2-byte LE, size-asserted | n/a | correct |
| `dump_yuv` (example) | `examples/dump_yuv.rs:33` | none — a dump tool the user diffs by hand | `u16` LE, documented as the **high-bit-depth companion** to `EC_AV1_PREFILT_DUMP` | not a gate; its doc names `yuv420p10le` | left alone — not a gate, and its contract is explicitly HBD |
| `assert_rawvideo_matches` | `stream.rs:5012` (pre-fix) | `aomdec --rawvideo` | `u16` unconditional | **NO** | **the defect** |

`decode_all_frames_vs_oracle` is the model the fix follows: it already derives the packing from
the decoded frame's own bit depth, and its oracle side is bit-depth correct in the instrument
script, so both sides agree at any depth.

## 2. Fix

Three changes, all in the `#[cfg(test)]` module of `crates/ec-av1/src/stream.rs`:

1. **`stream_bit_depth(stream, name) -> u8`** (new, `stream.rs:5016`): parses the stream's own
   sequence-header OBU and returns `color_config.bit_depth`. The depth comes from the bytes, not
   from the gate's name or recipe.
2. **`pack_rawvideo(decoded, bit_depth, name)`** (new, `stream.rs:5036`): packs Y, U, V per
   frame in the frame's own subsampled shape — 1 byte/sample at 8-bit, 2 bytes LE at 10/12-bit.
   At 8-bit it hard-asserts every sample `<= 0xff` and names the frame, so a decode that
   produced out-of-range samples is caught as "this is not an 8-bit stream", not as a pixel
   diff.
3. **`assert_rawvideo_matches(..., bit_depth: u8)`** (`stream.rs:5067`): the label is now an
   explicit argument, asserted against `stream_bit_depth` before anything else runs. A
   mismatched label panics naming both depths and the consequence ("packing for 10 compares
   nothing") instead of dying later on a raw-size mismatch. The size assertion now prints
   `{bit_depth}-bit`, our byte count, the sample count, and the oracle's byte count, so a size
   failure reads as a packing problem rather than a data bug.

An 8-bit call now **works correctly** rather than being refused: the u8 path is the tightly
packed planes, which is what `aomdec --rawvideo` emits. The loud failure is reserved for the
genuinely-wrong case (a label that contradicts the header), and it names the depths.

The one existing call site (`stream.rs:7056`) now passes `10`, which the new assert confirms
against the fixture's own header.

## 3. Proof, both directions

New gate: `the_rawvideo_helper_compares_real_samples_at_the_streams_own_bit_depth`
(`stream.rs:5144`). It builds a real `aomenc` 8-bit stream from an ffmpeg `gradients` source
(192x128, `--limit=6`, `--obu`), asserts the stream really is 8-bit from its parsed header, and
then runs **two** arms.

**Green-after (8-bit, compares real samples):**
```
the_rawvideo_helper_compares_real_samples_at_the_streams_own_bit_depth:
  6 8-bit frame(s) byte-identical to aomdec --rawvideo
test result: ok. 1 passed; 0 failed
```

**Wrong-label arm (must fail by name, not by size):** the same stream handed in with a
`10` label panics with
```
the stream's sequence header says 8-bit, the gate passed 10-bit -- aomdec's --rawvideo
writes 8 bits per sample for it, so packing for 10 compares nothing
```
caught by `catch_unwind` and asserted on its text. Without this arm the gate would be a green
that a reverted depth check would also give.

**Red-before (fix reverted, both symptoms return):** I set `pack_rawvideo`'s depth branch to
`if false` and deleted the depth assert, rebuilt, and reran. The 8-bit gate fails exactly the
way the handing lane measured:
```
10-bit raw sizes differ -- ours packs 442368 bytes (221184 samples at 10 bit/sample),
aomdec wrote 221184
  left: 442368
 right: 221184
...
a 10-bit label on an 8-bit stream must be refused by the depth check naming both depths;
the panic read: ... raw sizes differ ...
test result: FAILED
```
221184 vs 442368 is the same 2x raw-size class as the handing lane's 2359296 vs 1179648, at
this gate's smaller geometry. **Not one sample is compared on that path.** The fix was then
restored and both arms went green again.

**Unchanged high-bit-depth gate:** the helper's only pre-existing caller,
`a_lossless_444_128_root_lossless_stream_reads_chunks_chunk_major`, passes post-fix
(`test result: ok. 1 passed`). Its control arm still runs the same 6-frame `u16` LE compare
against `aomdec --rawvideo` — the new label argument matches what the packing already did, so
the arm's substance is untouched.

**Cross-check on the other helper:** `a_pinned_rect_stream_with_a_64x64_intra_block_in_an_inter_frame_decodes_pixel_exact`
(an 8-bit stream through `decode_all_frames_vs_oracle`) passes, confirming that helper's
depth-aware path is sound at 8-bit and is not part of the defect.

## 4. False greens

**None found, and the reason matters: the defect produced a hard failure, never a silent
pass.** The pre-fix helper asserted `ours.len() == ref_raw.len()` before comparing anything,
and a `u16` pack of an 8-bit stream is exactly 2x the oracle's size, so the assert always
fired. The 8-bit cell was a **loud, misleading failure**, not a vacuous green: the test
reported "raw sizes differ" and a reader would reasonably read that as a decoder or fixture
bug rather than as "this helper cannot judge this stream". No gate in the suite was reporting
a pass it had not earned through this helper.

What *was* silently absent: any 8-bit coverage of `assert_rawvideo_matches` itself. The only
caller was 10-bit, so the helper's 8-bit path had never been exercised and no test asserted
anything about it. The new gate closes that, and its wrong-label arm is what makes the depth
check itself non-vacuous.

Two adjacent traps checked and left alone, both outside this lane's scope (tests/helpers only,
and neither is a false green):

- `dump_yuv` (`examples/dump_yuv.rs`) packs `u16` unconditionally, like the old helper. It is
  a diagnostic dump tool, not a gate, and its docstring states it is the high-bit-depth
  companion to `EC_AV1_PREFILT_DUMP` and names `yuv420p10le` as the reference. Handing an
  8-bit stream to it produces a dump that will not `cmp` against `yuv420p10le` — same trap,
  but the contract is documented and nothing asserts on it. Flagging for a follow-up lane, not
  fixing here.
- `ffmpeg_decode_sequence*` all hard-code their own pix_fmt and size-assert before unpacking,
  so a depth mismatch is a loud failure there too.
