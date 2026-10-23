# av1mainfail: ec-av1 test failures on main

Base: main f938ce85. Run on server host a (slot free after queueing), log at ~/.cache/wt/av1mainfail-remote.log (on the local machine).
Command: EC_REQUIRE_FIXTURES=1, EC_AV1_AOM_DIR=$HOME/fixtures/av1-aom (set on the server side).

## Result: no reproduction

- `cargo test --release -p ec-av1 --lib --no-fail-fast`: test result ok. 818 passed; 0 failed; 49 ignored (3773 s). LIB_RC=0.
- `cargo test --release -p ec-av1 --test aom_conformance --no-fail-fast -- --nocapture`: `aom vectors: 242/242 exact (611 shown frames), 0 refused by name, 0 known failing, 0 failed`. test result ok. AOM_RC=0.

## Classification of the reported failures

- `aom_vectors_match_per_frame_md5` (line ~344): cause (a), missing env var. The failing assert is `EC_REQUIRE_FIXTURES=1 but EC_AV1_AOM_DIR is not set`. The test reads `EC_AV1_AOM_DIR`, not `~/fixtures` directly. All three server hosts have `~/fixtures/av1-aom/` with the 242 vectors. Without the variable, the test fails under EC_REQUIRE_FIXTURES=1. With it set, it passes with no MD5 mismatch.
- `--lib`: no failing test names found. The full lib run passes on the server, so the failure does not reproduce there. Its cause is unknown. Not run locally (see below).

## Proposed fix

Make the caller set the variable, not the test. Options:
1. Add `EC_AV1_AOM_DIR` to whatever runs the gate (CI or the lane script) pointing at `fixtures/av1-aom`.
2. Or have `aom_conformance` default to `<repo>/fixtures/av1-aom` when `EC_AV1_AOM_DIR` is unset, the same way other fixture tests resolve `../../fixtures`. This makes the test work under EC_REQUIRE_FIXTURES=1 without extra env.

Lib: re-run with the exact failing environment the reporter used. The names are still needed to fix it.

## Not done

- No bisect: nothing regressed on main.
- No local run: the local `fixtures/` has no `av1-1-b*.md5` files, so aom_conformance cannot run locally without copying the vectors. The lib run was left to the server (about 1 h CPU).
