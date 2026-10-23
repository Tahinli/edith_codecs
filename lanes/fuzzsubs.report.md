# fuzzsubs: cargo-fuzz targets for ec-subs, ec-ass, ec-pgs

Goal: DONE.md shared gate 2 for ec-subs (SRT, WebVTT, markup, IR conversion), ec-ass (script,
section, event and override-tag parsing) and ec-pgs (segment parsing, RLE decode, composition to
bitmaps). One standalone cargo-fuzz crate per crate, same layout as `crates/ec-ogg/fuzz/`.
Base sha: a19477ea (main at worktree creation).

## What changed
- `crates/ec-subs/fuzz/`: `ec-subs-fuzz`, targets `srt`, `webvtt`, `markup`, `ir`. Text targets run
  the input bytes and then the lossy-UTF-8 text. `webvtt` also tries a `WEBVTT` header in front.
  `markup` wraps the input in a one-cue SRT and WebVTT document so the shared tag stack is reached.
- `crates/ec-ass/fuzz/`: `ec-ass-fuzz`, targets `script` (whole document, bytes and lossy text),
  `styles` (first byte picks V4 or V4+ table, and whether a Format line is present), `events`
  (input as one Dialogue row), `overrides` (first byte picks WrapStyle 0 to 3; input is the Text field).
- `crates/ec-pgs/fuzz/`: `ec-pgs-fuzz`, targets `segments` (raw `.sup` bytes split in two at a
  point the first byte chooses), `rle` (structured ODS whose declared size comes from input),
  `compose` (input read as (kind, length, body) records framed as valid segments).
- `seed.sh` in each crate: ec-subs seeds from `fixtures/subs/real.srt` plus a generated WebVTT twin;
  ec-ass from `real.ass` (whole file, and its Dialogue and Style rows); ec-pgs from `pgs-1080p.sup`.
  `.gitignore` covers target, corpus, artifacts, coverage.
- Library fixes (all found by reading the code; none by fuzzing, see below):
  - `ec-subs/src/markup.rs`
    - `parse_clock`: the hours/minutes/seconds sums and the microsecond scaling are `checked_*`. A
      clock that does not fit an i64 of microseconds now returns `None` (the cue is skipped and
      counted) instead of panicking in debug.
    - Markup nesting capped at 64 open frames (`MAX_NEST`). Before, a cue of a million `<i>` built a
      tree that was quadratic to close (`rposition` over the stack per close tag) and too deep for
      `plain_text` and `Drop`.
    - Entity lookup for `&` scans at most 11 bytes for `;`. Before, each bare `&` scanned to the
      end of the cue, so a run of `&` was quadratic.
    - `utf16` decoding uses `as_chunks::<2>` (clippy; same result).
  - `ec-ass/src/lib.rs`
    - `alignment`: `(code - 1)` became `code.wrapping_sub(1) % 3 + 1`. `i32::MIN` (a number a style
      row or `\an` can state) panicked. Results are identical for every other input.
    - `WrapStyle` is read once from the header, not searched for on every Dialogue row (that search
      was quadratic on long headers). The first `WrapStyle:` line still wins, as before.
  - `ec-pgs/src/lib.rs`
    - Object dimensions above `MAX_DIMENSION` (4096) are refused with `Error::corrupt`. Before, an
      object declared 65535x65535 allocated its RLE bitmap at 4 GB. The oversized partial is dropped,
      later fragments under that id cannot attach to an older object, and the rest of the buffer is
      still consumed. `push` returns the first such error after the buffer is through, so a caller
      sees the refusal; this is a change to `push`'s error contract, documented in its doc comment.
    - Canvas above 4096 in either dimension is refused the same way (was a 17 GB RGBA `vec!`).
    - `take_frame` uses `VecDeque::pop_front` (was `Vec::remove(0)`, quadratic on a drained backlog).
- Tests (all in each crate's `tests/`, inputs inlined, no fixtures needed):
  - `crates/ec-subs/tests/fuzz_regressions.rs`: overflowing clock, cue with overflowing clock skipped,
    200k nested `<i>`/`<b>` (text survives), 100k bare `&` (linear and unchanged).
  - `crates/ec-ass/tests/fuzz_regressions.rs`: `-2147483648` alignment in a style row and in `\an`,
    Dialogue clock overflow skipped, first WrapStyle governs `\n`, 20k-line header with 5k rows.
  - `crates/ec-pgs/tests/fuzz_regressions.rs`: 65535x65535 object refused and the next legal set
    still renders, 65535x65535 canvas refused.
- `crates/ec-subs/tests/real_library.rs`: two `chunks_exact(4)` calls changed to
  `as_chunks::<4>().0.iter()` so `--all-targets` clippy is clean. Assertions untouched.

## Bugs found and fixed
Fuzzing found no crash, timeout or OOM in 120 s per target (see table). The fixes above come from
reading the parsers. Each one is an input the parser must handle that either panicked in debug
(overflow), allocated without a bound, or took quadratic time. The regression tests above cover each.

Not verified: I did not re-run the new regression tests against the pre-fix code to confirm they
fail there. The overflow and allocation cases are reasoned from the code; the tests pass on the fix.

## Verify commands and results (all via `scripts/remote-run.sh fuzzsubs`, host a or b)
- `cargo +nightly fuzz build --fuzz-dir crates/<c>/fuzz` for ec-subs, ec-ass, ec-pgs: exit 0, all 11 targets built.
- Fuzz, each target `cargo +nightly fuzz run --fuzz-dir crates/<c>/fuzz <t> -- -max_total_time=120 -rss_limit_mb=2048 -timeout=10`:

| target               | exit | executions | crash/timeout/OOM |
|----------------------|------|-----------:|-------------------|
| ec-subs srt          | 0    | 4,818      | none              |
| ec-subs webvtt       | 0    | 3,801      | none              |
| ec-subs markup       | 0    | 3,704      | none              |
| ec-subs ir           | 0    | 3,523      | none              |
| ec-ass script        | 0    | 10,345     | none              |
| ec-ass styles        | 0    | 955,789    | none              |
| ec-ass events        | 0    | 1,790,123  | none              |
| ec-ass overrides     | 0    | 1,449,491  | none              |
| ec-pgs segments      | 0    | 22,570     | none              |
| ec-pgs rle           | 0    | 17,524     | none              |
| ec-pgs compose       | 0    | 21,231     | none              |

  Fuzz runs used the code before the clippy-only edits (`as_chunks` in ec-pgs `pds`, ec-subs `utf16`,
  and the real_library test); those edits do not change behaviour and the release tests pass after them.
- `cargo clippy -p ec-subs -p ec-ass -p ec-pgs --all-targets -- -D warnings`: exit 0, clean.
- `cargo test --no-fail-fast -p ec-subs -p ec-ass -p ec-pgs` (debug): all suites pass; the real PGS
  fixture test takes about 30 s in debug.
- `cargo test --no-fail-fast --release -p ec-subs -p ec-ass -p ec-pgs`: exit 0. Counts: ec-ass 4 unit
  + 4 integration; ec-pgs 5 unit + 3 integration; ec-subs 13 unit + 4 regression + 2 real-library.
  The real-library PGS test needed `EC_REQUIRE_FIXTURES=1` and the fixture symlink.

## Design decisions to review
- PGS frame queue stays unbounded. A cap that drops the oldest frame broke the existing
  `a_real_pgs_stream_decodes_to_plausible_bitmaps` test (it pushes the whole file, then drains), so
  it was reverted. A caller that never calls `take_frame` holds every frame. The fuzz targets drain
  after every push.
- PGS object cap is 4096 in each dimension. The brief suggested this or the declared data length;
  the dimension cap is what is implemented. Declared data length is not separately checked.

## Left open
- The gate asks for 1 CPU-hour per target. Only 120 s smoke runs were done, so the gate is not met.
- Corpora are seeded from fixtures only. No minimized corpus or coverage was measured. Seed corpora
  are not committed (git-ignored); the server-side corpus and artifact directories did not sync back.
- The `ec-subs` targets run at 30-40 exec/s (full parse per input, two passes for text). Expected for
  this input shape, not a finding.
- The PGS RLE amplification (`render` decodes each placed object once per END segment) is not cached.
  Linear in segments, not a quadratic path; left as is.
- `fuzz/Cargo.lock` files generated on the server are not in this commit.
