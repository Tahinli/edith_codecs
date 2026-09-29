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

---

# r2 — rebase onto main and fix the stranded call site

**Rework reason (Main, from an independent review):** the 4-arg → 5-arg signature change
stranded a call site this branch never saw. `git merge-tree` reported the merge CLEAN and the
merged tree then failed to compile with E0061.

**The trap, stated once: a signature change must be validated by COMPILING the merged result,
not by `git merge-tree`'s conflict report.** `merge-tree` reasons about content overlap, not
about arity — a call site on lines the signature diff never touches is a non-conflict to it and
an E0061 to rustc.

## 1. Rebase and the full call-site set

Rebased `lane-av1rawhelper` onto current main `9b2f6c9ddb5c704e7649900c704b911e6822cd43`
(clean rebase, 1 commit replayed → `8bf6c4fe`). The call-site set was then re-established from
the CURRENT tree with `grep -rn "assert_rawvideo_matches(" crates/`, not from the branch's
pre-rebase view.

| | count | sites (rebased tree) |
|---|---|---|
| before r2 | 2 real call sites, 1 of them 4-arg stranded | `stream.rs:7071` (updated), `stream.rs:7165` (**stranded**, 4-arg) |
| after r2 | 2 real call sites, both 5-arg | `stream.rs:7071`, `stream.rs:7165` |

(The r1 gate contributes 2 more call sites of its own — `stream.rs:5232` and `:5247` — which
are the deliberate wrong-label and correct-label arms.)

**Depth of the stranded site, from the bytes.** `stream.rs:7165` belongs to
`a_lossless_block_clips_its_transform_grid_at_the_frame_edge`, added by `7b9f46fa`, which the
r1 base `7f8817cb` predates. Its depth is **not** taken from the gate's doc prose, which says
"128x96 yuv444p 4:4:4 lossless" without naming a depth. It is read off the pin's own parsed
sequence header via the crate's own parser:

```
$ cargo run -p ec-av1 --example decode_probe -- fixtures/ll444_128root_lossless.obu
SEQ: use_128x128_superblock=true bit_depth=10 mono_chrome=false max_frame=128x96
$ cargo run -p ec-av1 --example decode_probe -- fixtures/ll444_minp64_128root_control.obu
SEQ: use_128x128_superblock=true bit_depth=10 mono_chrome=false max_frame=128x96
```

Both pins are genuinely 10-bit, so both sites take `10` — and the new in-helper assert
independently re-derives and confirms that at runtime, so a wrong label here could not pass
silently.

## 2. Both gates green (tails)

```
$ cargo test -p ec-av1 --lib the_rawvideo_helper_compares_real_samples -- --nocapture
the_rawvideo_helper_compares_real_samples_at_the_streams_own_bit_depth:
  6 8-bit frame(s) byte-identical to aomdec --rawvideo
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 742 filtered out
```

```
$ cargo test -p ec-av1 --lib a_lossless_block_clips_its_transform_grid_at_the_frame_edge -- --nocapture
running 1 test
test stream::tests::a_lossless_block_clips_its_transform_grid_at_the_frame_edge ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 742 filtered out
```

The second tail is the one that matters: `a_lossless_block_clips_its_transform_grid_at_the_frame_edge`
is the gate that owns the stranded site, so its green is the proof that the site the branch
never saw is now correct — and it is a real 6-frame `aomdec --rawvideo` compare at 10-bit, not
a compile-only pass.

## 3. `cargo check -p ec-av1 --all-targets`

**CLEAN.** No errors, no warnings:

```
$ touch crates/ec-av1/src/stream.rs && cargo check -p ec-av1 --all-targets
    Checking ec-av1 v0.1.0 (/home/tahinli/.cache/wt/av1rawhelper/crates/ec-av1)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.36s
```

(The `touch` is load-bearing: an earlier run reported `Finished in 0.01s` off a cached binary,
which is not evidence of anything.)

## 4. The merged result itself, compiled

Checking the branch alone is not the check — the defect only exists in the MERGED tree. So the
merge was materialised and compiled:

```
$ git merge-tree --write-tree main HEAD          # tree 7125a2a3
$ git commit-tree 7125a2a3 -p main -p HEAD -m "merge-sim"   # 72954a27
$ git worktree add --detach ~/.cache/wt/av1rawhelper-merge 72954a27
$ cargo check -p ec-av1 --all-targets
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.81s
```

**The merged result compiles clean.** Its call sites are the ones the branch intended:

```
5079:    fn assert_rawvideo_matches(
5232:            assert_rawvideo_matches(&obu, &stream, NAME, frames, 10);
5247:        assert_rawvideo_matches(&obu, &stream, NAME, frames, 8);
7071:            assert_rawvideo_matches(&ctrl, &ctrl_stream, NAME, FRAMES, 10);
7165:            assert_rawvideo_matches(&obu, &stream, NAME, FRAMES, 10);
```

### Red-before for the rework

With `stream.rs:7165` reverted to its 4-arg form inside the merged tree, the merged result
fails at exactly the line and error the review reported:

```
error[E0061]: this function takes 5 arguments but 4 arguments were supplied
    --> crates/ec-av1/src/stream.rs:7165:13
     |
7165 |             assert_rawvideo_matches(&obu, &stream, NAME, FRAMES);
     |             ^^^^^^^^^^^^^^^^^^^^^^^------------------------ argument #5 of type `u8` is missing
error: could not compile `ec-av1` (lib test) due to 1 previous error
```

The line and the error match the review's finding, and `git merge-tree` had called this same
merge CLEAN. Restored to the 5-arg form it compiles clean again; the scratch merge worktree and
its target dir were removed.

## 5. Process note

Any lane that changes a function signature in this crate should, before reporting done:
establish the call-site set from the current tree, and then compile a materialised merge
(`git merge-tree --write-tree` + `git commit-tree` + worktree + `cargo check`) rather than
trusting the conflict report. Signature churn during a multi-lane merge wave is the exact
condition under which `merge-tree` is confidently wrong.

## r2 second pass — main moved again mid-verification

Main advanced 3 merges (`4532caa6` lane-av1pinslive, `bd588822` lane-av1leftwit, `1cc2f250`
lane-av1txsearch, landing as `8dd07ed7`) between the first r2 rebase and the end of it. Rebased
again onto `8dd07ed7` (3 commits replayed) and re-established the call-site set from the new
tip: **still exactly 2 real call sites** (`stream.rs:7071`, `stream.rs:7165`), both now 5-arg on
the branch, and main's own two sites (`:6889`, `:6983`) remain 4-arg for the merge to resolve.
Nothing new was stranded by the second wave.

Re-verified on the rebased tip:
- `cargo check -p ec-av1 --all-targets` — clean, no warnings
- `the_rawvideo_helper_compares_real_samples_at_the_streams_own_bit_depth` — ok, 1 passed
- `a_lossless_block_clips_its_transform_grid_at_the_frame_edge` (owner of the stranded site) —
  ok, 1 passed
- merged result materialised again against the new main
  (`merge-tree` tree `e2bb41c8` → merge commit `f8c7eaec`) and compiled:
  `Finished dev profile in 7.74s`, with all four call sites in the 5-arg form

Final branch tip: `a5584e43` (report) over `f6d45be0` (stranded-site fix) over `970c72cf`
(the fix itself). Not pushed.

### A second stale-binary trap, same family

`cargo test` reported `0 passed ... 715 filtered out` for
`a_lossless_block_clips_its_transform_grid_at_the_frame_edge` even though the test existed in
the source at `stream.rs:7117` — a stale test binary from before the rebase, with 715 tests
instead of 747. `touch crates/ec-av1/src/stream.rs` before the run restored it. Two independent
`cargo test` invocations in this lane returned confident green-looking summaries
(`0 passed, 0 failed` and `Finished in 0.01s`) that were both **serving a stale binary**, so a
run that reports fewer tests than the previous run is evidence of nothing until the file is
touched. Same shape as the `merge-tree` lesson: a tool reporting a clean result is not the same
as the result being correct.
