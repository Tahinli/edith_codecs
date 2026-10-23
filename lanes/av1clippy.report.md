# lane av1clippy: ec-av1 clippy gate (DONE.md "ec-av1 decode")

## Goal
`cargo clippy -p ec-av1 --all-targets -- -D warnings` must pass. **It does not pass yet.**

## Base
main @ 95bc8a76

## What changed
1. `clippy::uninit_vec` in `crates/ec-av1/src/film_grain.rs` (`uninit_plane`): replaced
   `Vec::with_capacity` + `set_len` with `vec![0; n]`. This is `alloc_zeroed`, which for a
   plane-sized buffer is a lazily-zeroed calloc, so no full-buffer memset is added. Not measured on
   a 4K frame. The unsafe block is gone and the doc comment is updated.
2. `clippy::uninit_vec` in `crates/ec-av1/src/decode.rs` (`fresh_plane`, env-gated
   `EC_AV1_PLANE_ALLOC=uninit|padzero|extzero` arm): kept the `set_len`, because that arm is a
   diagnostic that reads uninitialised planes on purpose. Added
   `#[allow(unsafe_code, clippy::uninit_vec)]` with a reason comment. The shipped arms return before
   reaching it. Judgment call: flagged for the orchestrator.
3. `clippy::mut_from_ref` in `crates/ec-av1/src/par.rs` (`Shared::get`): `#[allow(clippy::mut_from_ref)]`
   with a reason comment. Soundness is the `unsafe` band-disjointness contract documented on the fn.
4. Mechanical lints: `cargo clippy --fix --allow-dirty --all-targets -p ec-av1` ran on host a and
   applied the machine-applicable suggestions, mostly `needless_borrow`, plus `unnecessary_cast`,
   `useless_conversion`, `clone_on_copy`, `manual_is_multiple_of`, and similar. It touched 16 files
   under `crates/ec-av1/src` and `examples/` (alloc_probe.rs, dump_yuv.rs). `examples/decode_probe.rs`
   was rejected by the fix engine (its own suggestion was rolled back), and the file is unchanged.

## Lint counts
- Baseline (`-D warnings`, lib test target): **642** errors, 2 of them `uninit_vec`, 1 `mut_from_ref`.
- After the fix pass and the manual fixes above: **321** lint errors. `uninit_vec` and `mut_from_ref`
  are gone. Clippy still stops at `ec-av1 (lib test)`, so the gate fails.
- Left for the next round (over the ~40-lint threshold, so not done here): type_complexity (60),
  too_many_arguments (~70 across 7-14 args), doc_lazy_continuation (32), empty_line_after_doc (12),
  collapsible_if (21), operator_precedence (9), chunks_exact_to_as_chunks (8), needless_range_loop
  (~47), unused mut (5), manual_div_ceil (5), manual_is_multiple_of (4), and a long tail of 1-2 each.

## Verify
1. `scripts/remote-run.sh av1clippy -- cargo clippy -p ec-av1 --all-targets -- -D warnings`
   **FAIL**: 321 lint errors (see above). Log: `~/.cache/av1clippy-clippy3.log`.
2. `scripts/remote-run.sh av1clippy -- cargo test -p ec-av1 --release --lib -- --test-threads=2`
   **795 passed, 12 failed, 49 ignored** (2366 s, host a).
   The 12 failures (all the same cause, see below):
   - encode::tests::real_clip_encodes_within_its_quality_and_size_budget
   - encode::tests::real_clip_encodes_within_its_quality_and_size_budget_at_a_straddle_size
   - encode::tests::the_encoders_own_streams_are_byte_identical_to_their_pins
   - encoder::tests::a_1080p_multi_tile_stream_decodes_sample_exact_through_both_decoders
   - encoder::tests::bitrate_target_lands_within_5_percent_over_48_frames
   - encoder::tests::bytes_per_frame_controller_never_oscillates_past_its_clamp
   - encoder::tests::bytes_per_frame_target_settles_within_20_percent
   - encoder::tests::filter_replay_final_matches_the_capture_decode
   - encoder::tests::quality_target_is_monotone_in_bytes_and_psnr
   - encoder::tests::the_facade_codes_the_same_bytes_as_encode_sequence
   - stream::tests::a_libaom_stream_with_128_intra_blocks_in_inter_frames_decodes_exact
   - stream::tests::a_straddling_frame_decodes_exactly_on_both_reference_encoders_ladders

   **Cause (not in this diff):** each panics in `crates/ec-av1/src/library_fixture.rs:110` with
   "the library clip is absent at .../fixtures/video/h264-1080p-23.976-8bit.mp4" under
   `EC_REQUIRE_FIXTURES=1`. On host a, the lane's `fixtures` entry is a dangling symlink to this
   machine's `/home/user/.../fixtures`. `remote-run.sh` rsyncs the worktree's `fixtures` symlink
   (the exclude is for the directory only) and never replaces an existing link. The clip does exist
   at `~/fixtures` on both hosts. Infrastructure issue for the orchestrator.

   **Same 12 tests, run one at a time with the same env:**
   - branch: 12/12 pass (ran locally, after both server slots stayed busy for 10 min)
   - main: 12/12 pass (`~/.cache/wt/av1clippy-main`, lane av1main, host a)
   - The full main run was stopped on the coordinator's instruction, so the full-suite comparison on main is
     not available. The 12 failures are attributed to the fixture link, not to the diff, on the
     evidence of the per-test passes on both trees.

   Test log: `~/.cache/av1clippy-test1.log`. Per-test logs: `~/.cache/av1clippy-branch-tests.log`,
   `~/.cache/av1clippy-main-tests.log`.

## Left
- The `-D warnings` gate needs ~321 more lint fixes (list above). Decide whether to continue in
  this lane or split it.
- Fix the `fixtures` symlink sync in `scripts/remote-run.sh` (exclude the link, or recreate it on the host).
- `lanes/ORCHESTRATION.md` asks for the `Co-Authored-By: Claude Opus 5.5` trailer. The commit uses
  the `Claude Haiku 5.5` trailer from the attribution instruction in this session. Pick one.
