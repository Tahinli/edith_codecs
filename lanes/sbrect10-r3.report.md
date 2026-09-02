# lane-sbrect10 r3 report

Branch `lane-sbrect10`. Round input: r2 tip `1866bd5`, suite RED with 3 failed gates.
Round output: all three root-caused and green; merged main `85887c7` (which now carries
lane-mvtwin `2e2a225`), mvstack.rs resolved to main's copy byte-for-byte.

## 1. `a_real_aomenc_stream_with_film_grain_decodes_pixel_exact` -- ENVIRONMENT, not code

Panic body (r2 log had none: the unit was still running when the handoff was written, my first
`systemctl --user is-active sbrect10-suite-r2` used the un-timestamped unit name and read back
`inactive` wrongly). Run alone off the r2 binary:

    stream.rs:15680: called `Result::unwrap()` on an `Err` value:
    Os { code: 2, kind: NotFound, message: "No such file or directory" }

The worktree had **no top-level `fixtures` symlink** (`fixtures` is gitignored and per-worktree).
`fixtures/golden6-mismatch.obu` was simply absent. Not a temp-path collision: this gate writes no
temp file at all -- `ffmpeg_decode_sequence` (stream.rs:1316) pipes the stream through ffmpeg's
stdin/stdout, and the two helpers that DO make scratch dirs already key them on
`{test name}-{std::process::id()}` (stream.rs:2000, 15735), which is unique per process and per test.
Same failure at the same wall-clock in lane-sb128c's suite = the same missing symlink there, not a
race. Fixed with `ln -s .../edith_codecs/fixtures fixtures`.

SWEEP: all three `../../fixtures/*.obu` reads in `crates/ec-av1/src` (golden6-mismatch.obu,
golden7-forwarding-mismatch.obu, lr-sgr-r7.obu) now resolve.

EVIDENCE: $HOME/.cache/sbrect10-suite-r2.log + rerun | ln -s fixtures; ec_av1 binary --exact
a_real_aomenc_stream_with_film_grain_decodes_pixel_exact | FAILED (NotFound) -> ok, 0.31s

## 2. The other two reds: ONE cause -- r2's fixture swap landed in the sibling gate

Neither red was a pixel mismatch; both were vacuity asserts, and both counters read 0 for the
same reason. r2's half-random lavfi source

    horizontal: if(lt(X,128), 40+mod(floor((Y+N*sp)/32)*90,200), mod((X*7+Y*13+N*97)*31,256))
    vertical:   if(lt(Y,64),  40+mod(floor((X+N*sp)/32)*90,200), mod((X*7+Y*13+N*97)*31,256))

was written into `a_real_aomenc_inter_sequence_with_a_superblock_level_rect_partition_decodes_pixel_exact`
(stream.rs:6592) instead of `a_real_aomenc_inter_frame_with_a_64x64_intra_block_reads_the_no_cfl_uv_alphabet`
(stream.rs:6791) -- the rationale comment travelled with it, so the code read consistent. Effect:

- CFL gate kept r1's flat `200` source, whose right-hand superblocks aomenc codes skip ->
  8-bit arm decoded 0 intra blocks above 32x32: *"no attempt decoded an intra block above 32x32
  ... gate proved nothing (2 refusals, 14 attempts carried none)"*.
- rect gate got the textured source, which changed the RD's residual decisions -> its 10-bit arm
  lost the coded 64-axis transform unit lane-r14 r2 had measured on the flat source
  (8-bit residual TUs=1, 10-bit=0): *"no superblock-level rect strip carried a coded 64-axis
  residual transform unit ... (horz=2, vert=0)"*.

FIX (`e2d9e9e`): sources swapped back so each gate runs the source it was measured on. No assert
weakened, no grid changed, no decoder code touched.

Result of the two gates alone at the merged tree (`--test-threads=1 --exact`):

| gate | 8-bit | 10-bit |
|---|---|---|
| no_cfl_uv_alphabet | 4 refusals, 4 exact attempts, **15** no-CFL uv_mode reads | 0 refusals, 8 exact attempts, **43** reads |
| superblock_level_rect_partition | 2 refusals, 4 exact, 64x32=4 32x64=4, **64-axis residual TUs=2** | 2 refusals, 2 exact, 64x32=2 32x64=2, **residual TUs=2** |

0 out-of-scope mismatches in either depth of either gate, so r2's `combine_compound_candidates`
global-motion tail fill is NOT a regression source -- every attempt that decoded compared exact.

EVIDENCE: $HOME/.cache/sbrect10-g2-a_real_aomenc_inter_frame_with_a_64x64_intra_block_reads_the_no_cfl_uv_alphabet.log,
$HOME/.cache/sbrect10-g2-a_real_aomenc_inter_sequence_with_a_superblock_level_rect_partition_decodes_pixel_exact.log |
swap the two gates' geq sources, rebuild, run each gate alone under systemd-run MemoryMax=6G |
both `test result: ok`, counters 15/43 no-CFL reads and 2/2 64-axis residual TUs

## 3. Merge

`git merge --no-commit main` twice: first main `4eff3a1` (`de045b7`, clean auto-merge), then main
`85887c7` after it took lane-mvtwin mid-round. Only conflict: `crates/ec-av1/src/mvstack.rs`,
resolved to MAIN's copy byte-for-byte (`git diff main -- crates/ec-av1/src/mvstack.rs` EMPTY) --
main carries this lane's own comp_list global-motion tail fill plus mvtwin's insertion-time
candidate cap and tile-origin clamp. `cdf.rs`, `cdf_state.rs`, `refusal_inventory.rs` all
byte-identical to main. Lines this branch drops vs main are only its own hunks (uv_mode_cfl
narrowing, indexed `dump_stage16` prefix, gate fixtures). The merge commit was recorded by the
coordinator as `10e25e3` (parents `e2d9e9e` + `85887c7`) while this round was mid-flight.

## 4. Suite

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j3` under user unit
`sbrect10-suite-r3-1788326330.service`, log `$HOME/.cache/sbrect10-suite-r3.log`.
r2 for comparison: 368 passed / 3 failed / 32 ignored (the three above).

**GREEN: 378 passed / 0 failed / 33 ignored / 0 measured, 1229.56s.**

EVIDENCE: $HOME/.cache/sbrect10-suite-r3.log | systemd-run --user --unit=sbrect10-suite-r3-1788326330
-p MemoryMax=10G cargo test -p ec-av1 --lib -j3, EC_AV1_REQUIRE_AOMENC=1 |
`test result: ok. 378 passed; 0 failed; 33 ignored` (r2 was 368/3/32)

## 5. Residue

- accepted: the no-CFL gate's 8-bit arm still takes named refusals on other shapes
  (SB-level AB partition, 32x64/64x32 split intra strips) -- other lanes' scope, and 4 attempts
  decode + prove the shape regardless.
- accepted: r2's `probe.sh` counted intra-in-inter blocks through an `EC_AV1_TELL` trace grep,
  not through `nocfl_uv_mode_hits`; the gate itself asserts the real counter, so the proxy is only
  a probe convenience. Not worth a rewrite.
