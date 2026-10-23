# testcut Phase 2: evidence-gated deletion of heavy tests (protocol)

Scope: only the heavy tests (≥ 20 s in `lanes/testcut/{av1,rest}.tsv`), meaning the ones that `cargo nextest run --profile fast` excludes. Light tests are never deleted, because removing them saves nothing (Phase 0: all 1850 sub-second tests together take 196 s).

A heavy test `T` is deleted only when evidence shows the rest of the suite already detects what `T` detects. The method follows Shi FSE'14: reduce against mutant kills, not coverage alone.

## Protected (never deleted, record as "protected")
- Conformance or reference-vector suites (JVT, JCT-VC, RFC vectors, libvpx/xiph vectors, SEI hash, Argon or aom vectors).
- Named fuzz-crash regressions and golden-checksum tests.
- Tests named in `DONE.md` acceptance criteria (`grep -n <name> DONE.md lanes/*.charter.md`).
- Wall-clock or perf tripwires (names like `*_faster_*`, `*realtime*`, `beats_the_incumbent`, `speedup_curve`). These are not correctness tests; Phase 3 already keeps them out of `fast`.

## Steps per candidate T (all heavy compute on server via `scripts/remote-run.sh <lane> --host auto -- …`)
1. **Read T.**
   - If it has no assertion that can fail on wrong output (it only prints, or only asserts "no panic" where light tests already decode the same inputs), mark it **pseudo-test**.
   - Pseudo-tests skip to step 4 using only the mutation run.
2. **Coverage.** Use `cargo llvm-cov nextest --release -p <crate> --no-report -E 'test(=T)'` to collect T's region coverage, then `cargo llvm-cov report --json`. Collect the coverage of the fast suite of the same crate the same way (`--profile fast`).
   - `U(T)` = regions T covers that the fast suite does not.
   - If `U(T)` is empty, T is a candidate.
   - If `U(T)` is not empty, mutate only the functions containing `U(T)` in step 3. Those are T's unique regions.
3. **Mutation kill check.**
   - List the mutants: `cargo mutants -p <crate> --list --json`. Filter them to the functions T covers, preferring the `U(T)` functions and then the functions with the lowest fast-suite hit counts. Sample at most **60 mutants** per candidate.
   - Run them with `cargo mutants -p <crate> --test-tool nextest --re '<fn regex>' --timeout-multiplier 3 --baseline skip`, once with the test set *fast suite* and once with the test set *T only*. Narrow the test set with `--cargo-test-arg=--profile --cargo-test-arg=fast` for the first run and `-E 'test(=T)'` for the second.
   - `S` = mutants that survive the fast suite. `K` = the mutants in `S` that T kills.
   - Run every mutant in `K` against the **other remaining heavy tests** of the crate. `K'` = the mutants in `K` that no other remaining heavy test kills.
4. **Decision.**
   - If `K'` is empty and T is not protected, delete T (whole function and any helpers left unused).
   - If `K'` is not empty, keep T, and record `|K'|` and one example mutant in the report.
   - Pseudo-tests that kill nothing are deleted. Pseudo-tests that kill something are kept and noted as "weak oracle, kills N".
   - Process the candidates in decreasing runtime. When one is deleted, it no longer counts as "remaining" for later candidates.
5. **Verify.**
   - The crate's gate passes with the deletions: `EC_REQUIRE_FIXTURES=1 cargo test --release -p <crate>`. For ec-av1, also pass `EC_MEMGUARD_MAX=12G EC_AV1_AOM_DIR=$HOME/fixtures/av1-aom … -- --test-threads=4`.
   - `cargo clippy --release -p <crate> --tests -- -D warnings` is clean.
   - `forbid(unsafe_code)` is still present.
   - Regenerate the fast profile with `scripts/gen-fast-profile.sh` only if the toml is touched. Removing deleted names from the filter is optional, since a filter that matches nothing is harmless.

## Report row (append to the lane report file the orchestrator names)
`| crate | test | seconds | class (protected/pseudo/candidate) | |U(T)| | mutants tried | |S| | |K| | |K'| | decision |`

## Rules
- Never run hardware encode (ec-hw, ec-va).
- Never pull, push or merge.
- Never touch `crates/app`.
- Commit on your lane branch with Cargo.lock.
- Use a unique `remote-run` lane name and run `--clean` at the end.
- If a step's tooling fails three times, stop and report what failed rather than guessing. Never delete a test without its kill evidence.

## Batched variant for crates with many candidates (ec-av1)
Per-candidate mutation runs don't scale to 143 tests, so build one kill matrix per lane:
1. **Per-test coverage of the lane's heavy tests in one instrumented pass.** nextest runs each test in its own process, so set `LLVM_PROFILE_FILE=<dir>/%p-%m.profraw`. Run the tests one at a time (`-j1`) and record which pid ran which test, or else run one `cargo llvm-cov nextest --no-report -E 'test(=T)'` per test and copy out the profraw files after each.
2. **Mutant sample.**
   - Take `cargo mutants -p ec-av1 --list --json` and restrict it to files and functions the lane's heavy tests cover.
   - Sample **≤ 200** mutants, stratified per file (`--shard k/n` order or a seeded random pick; record the seed).
   - Run the sample against the fast suite: `cargo mutants … --in-place` is not allowed; use the normal copy mode with `--test-tool nextest` and the fast profile. `S` = the survivors.
3. **Kill matrix over S × heavy tests.**
   - Per surviving mutant: apply its diff (`cargo mutants --list --diff`) in a scratch copy, build once, and run all of the lane's heavy tests with `cargo nextest run --no-fail-fast -E '<heavy set>'`.
   - Record the failing tests as a row of `lanes/testcut/<lane>.killmatrix.tsv` (mutant, killing tests).
   - The tool cache (`EC_TOOL_CACHE_DIR`) keeps oracle cost low.
4. **Runtime-weighted greedy set cover over the matrix** (Smith & Kapfhammer): repeatedly keep the test with the most uncovered kills per second.
   - Tests outside the cover are **candidates**.
   - A candidate is deleted only if all of these hold:
     - it kills nothing that the kept set misses;
     - its coverage is a subset of (fast suite ∪ kept heavy tests);
     - it is not protected.
   - A candidate with unique coverage is kept, because the mutant sample is too small to clear it.
5. **Budget.** At most about 10 host-hours per lane. Spread the work across hosts with `remote-run --host auto` and several lane names (e.g. `<lane>-m1`, `<lane>-m2`). Shard the mutants with `--shard k/n`.
