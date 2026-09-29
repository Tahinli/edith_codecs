# lane-av1ignorereasons — every bare `#[ignore]` in `crates/` now names its unblock

Branch: `lane-av1ignorereasons` (worktree `/home/tahinli/.cache/wt/av1ignorereasons`), base `3aa16dd1`.

## Result

All 15 bare `#[ignore]` attributes in the repo live in **one file**:
`crates/ec-opus/tests/conformance.rs`. Every one now carries a reason string.
**No test was un-ignored** — see "Class (b): none" below for the evidence per candidate.

```
$ grep -rE '#\[ignore\]\s*$' crates/*/src crates/*/tests | wc -l
before: 15
after:  0
$ grep -rE '#\[ignore = ' crates/*/src crates/*/tests | wc -l
before: 65
after:  80
```

## Class (b): none — why nothing was un-ignored

Three candidates were examined for "blocker already passed". All three fail on a
**corpus/runtime dependency, not on a defect**, so un-ignoring them would have
baked a 12-minute local-media sweep into the default suite:

| candidate | what would have to be true to un-ignore | measured |
|---|---|---|
| `encoder_library_gate_vs_libopus` | the sadie@64k dropout that failed it is fixed | **the defect is fixed** — `lanes/opus-drop-r2.report.md`: "sadie@64 minsec .8987→.9148, drops 1→0; 14-row 0 ours-only dropout seconds", root-caused to reservoir arithmetic in `celt_enc.rs` VBR. But the sweep is 7 sources × {64,96} kbps × 120 s of ffmpeg libopus encodes (~12 min per `opus-gate-r1.report.md`) over `~/Music` + `~/Downloads`, and it `SKIP`s missing sources — un-ignoring it would leave a gate that silently measures 6 of 7 rows. Blocked on the corpus being a committed fixture, not on a defect. |
| `silk_library_gate_vs_libopus` | sadie.wav present | `assert!(src.exists())` on `~/Music/sadie.wav`; **the file is gone from this host** (`ls ~/Music/sadie.wav` → No such file). Hard-fails today. |
| `celt_click_peak_offset` | a real tolerance to assert | Ran it: `celt click {32000,64000,128000}: peak +120 (lm 3)` — a stable +120-sample offset, but the body has **zero asserts** (pure `println!`). Un-ignoring would add a permanently-green no-op. It stays a probe with the unblock named (turn the offset into a tolerance assert). |

The measured blocker class here is "local media corpus is not a committed fixture",
which is a corpus/provenance problem, not a test-reason problem — hence reasons
naming the corpus rather than reasons naming a fixed defect.

## Per-item table

| path:line | class | decision | reason text (abbreviated) |
|---|---|---|---|
| `conformance.rs:778` `real_library_sweep` | (a) probe | reason added | probe: report harness with no asserts; unblocks by setting `EC_OPUS_FILES` to committed `.opus` fixtures — unset, it decodes nothing |
| `:3004` `sadie64_persecond_diag` | (a) probe | reason added | per-second correlation dump for the 64k lane; unblocks when `~/Music/sadie.wav` is back in the corpus (hard-asserts `src.exists`) |
| `:3263` `encoder_library_gate_vs_libopus` | (c) gate, corpus-blocked | reason added | library gate: hand-run 14-row sweep, 120 s/source, needs ffmpeg libopus + local corpus (~12 min); the sadie@64k dropout that first failed it is fixed per `lanes/opus-drop-r2.report.md`, so only the corpus dependency keeps it out of the default suite |
| `:3575` `silk_library_gate_vs_libopus` | (c) gate, corpus-blocked | reason added | library gate: hand-run mono VoIP sweep; blocked until `~/Music/sadie.wav` is back in the corpus (hard-asserts `src.exists`), and it is a multi-minute ffmpeg run, not a suite test |
| `:3814` `silk_silkq_persecond_diag` | (a) probe | reason added | per-second SILK 12k bit-allocation dump; unblocks when `~/Music/sadie.wav` is back in the corpus |
| `:4086` `silk_silkq_oracle` | (a) probe | reason added | per-frame SILK `Indices` side-by-side oracle dump; unblocks when `~/Music/sadie.wav` is back in the corpus |
| `:4368` `silk_spectral_divergence_12k` | (a) probe | reason added | per-band spectral divergence dump at 12k; unblocks when `~/Music/sadie.wav` is back in the corpus |
| `:4724` `opus_compare_harness` | (a) probe | reason added | needs hand-supplied `SW_REF`/`SW_TEST` `.sw` files to cross-check our `opus_compare` against the C tool; asserts nothing without those env vars |
| `:4760` `opus_compare_harness_ours` | (a) probe | reason added | needs hand-supplied `SW_SRC`/`SW_KBPS`/`SW_OUT_SRC`/`SW_OUT_DEC`; only writes `.sw` dumps, asserts nothing |
| `:4903` `spectral_divergence_vs_libopus` | (a) probe | reason added | per-window CELT band divergence sweep over the local corpus; a multi-minute ffmpeg report run, not a suite gate |
| `:5385` `naz_startup_hop_energies` | (a) probe | reason added | `HOP_SRC`/`HOP_MS`-driven hop-energy dump; returns silently when `HOP_SRC` is absent, so it can never be a suite gate |
| `:5468` `celt_click_peak_offset` | (a) probe | reason added (ran it) | self-contained, but prints where a synthetic click lands after a CELT-only roundtrip and asserts nothing; unblocks when that offset is turned into a tolerance assert |
| `:5499` `short_block_bits_vs_libopus` | (a) probe | reason added | per-frame short-block allocation dump over the local corpus; a multi-minute ffmpeg report run, not a suite gate |
| `:5780` `err_map_vs_libopus` | (a) probe | reason added | `EC_ERRMAP_SRC`-driven per-window error map; returns silently when the named source is missing, so it can never be a suite gate |
| `:6034` `frame_decisions_vs_libopus` | (a) probe | reason added | `FRAME_SRC`/`FRAME_FROM`/`FRAME_TO`-driven per-frame decision dump; returns silently when `FRAME_SRC` is missing, so it can never be a suite gate |

## Run output

No test was un-ignored, so the only run is the one probe whose viability was in
question (it is the only body with no external dependency, so it was the only one
that could be probed cheaply).

```
$ cargo test -p ec-opus --test conformance --release -- --ignored celt_click_peak_offset --nocapture
running 1 test
celt click 32000: peak +120 (lm 3)
celt click 64000: peak +120 (lm 3)
celt click 128000: peak +120 (lm 3)
test celt_click_peak_offset ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 43 filtered out; finished in 0.00s
```

The +120 offset is stable across all three bitrates, i.e. the measurement is sound;
what it lacks is a threshold. That is the unblock the reason now names.

## Suite + build

```
$ cargo check -p ec-opus --all-targets
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.35s

$ cargo test -p ec-opus
test result: ok. 35 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s   # lib
test result: ok. 28 passed; 0 failed; 16 ignored; 0 measured; 0 filtered out; finished in 34.79s # conformance
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

The 16 ignored (15 bare + 1 pre-existing with a reason) now print their reason in
the test output, e.g. `test err_map_vs_libopus ... ignored, probe: EC_ERRMAP_SRC-driven
per-window error map; ...`.

## Left alone

- The 65 pre-existing `#[ignore = "..."]` attributes were not touched. No reason was
  found to be provably stale: each names a corpus file, an env var, a fix target, or
  an explicit "long sweep" cost, and the ones that name a defect (e.g. the
  `enc-dropout-lowrate-transient` debt in `opus-gate-r1.report.md`) are superseded by
  the later `opus-drop-r2` fix rather than stale-on-their-own — the gate that carried
  that debt is the one whose reason I rewrote to record the fix.
- No test body was modified; the change is 15 attribute lines only
  (`git diff --stat`: 1 file, 15 insertions, 15 deletions).
