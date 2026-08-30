# lane-realworld r3 report

VERDICT: Job 1 (part64 silent desync) FIXED, tested, committed (879999d).
Job 2 (delta_q/delta_lf) UNSTARTED but fully scoped below — turn budget ran
out during the design phase, before any code was written, so there is
nothing half-done to unwind for the next agent.

## Job 1 -- part64 silent desync (committed 879999d)
`decode_inter_frame_tile_with_cdfs` (decode.rs, the `(true, true)` arm around
what was line 9397) read `part64` then `let _ = part64;`, always recursing
the quadrant loop as if SPLIT. Fixed: the value is now kept (still inferred
as `PARTITION_SPLIT` for the three straddle/forced-half cases, matching the
intra tile's own three-way write), and if a real read comes back anything
other than `PARTITION_SPLIT` the function returns
`Err(unsupported("an inter SB-level partition type other than SPLIT (this
decoder's inter tile path only recurses a superblock as SPLIT)"))` before
the quadrant recursion runs.

**New refusal string** (verbatim, for the merge's `refusal_inventory.rs`):
`"an inter SB-level partition type other than SPLIT (this decoder's inter
tile path only recurses a superblock as SPLIT)"`

Test: `decode::tests::a_non_split_inter_sb_partition_refuses_by_name_instead_of_desyncing`
hand-writes a single `PARTITION_HORZ` `part64` symbol with `SymbolEncoder`
and asserts `decode_inter_frame_tile` returns that named error rather than
decoding garbage. `EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j4`:
**234 passed, 0 failed, 17 ignored** (was 232/0/17 before this lane; +1 this
test, +1 from r2's CDEF work already on the branch).

No gate needed for this one (charter: "make it refuse by name... and prove
the refusal with a test" -- minimum rung taken deliberately, not the
"handle it" rung, since real non-SPLIT part64 content requires the same
multi-SB, non-`--cpu-used=0` recipe r2 already flagged as its own separate
lane, not this desync fix).

## Job 2 -- delta_q/delta_lf: scoped, not started

Ran the libaom source at `~/.cache/aom-oracle/src/av1/decoder/decodemv.c`
and `av1/common/entropymode.c` directly (real oracle, not memory) before
running out of budget. Concrete plan for the next agent:

### Default CDFs (verified against entropymode.c:840-852)
Both are 4-symbol alphabets (`DELTA_Q_PROBS`/`DELTA_LF_PROBS` = 3), and this
crate's CDF convention already matches libaom's `AOM_CDF4` arguments
directly (confirmed against `cdf.rs`'s existing `SKIP` table, which is
`AOM_CDF2(31671)` verbatim) -- no inversion needed:
```rust
pub const DELTA_Q: [u16; 5] = [28160, 32120, 32677, 32768, 0];
pub const DELTA_LF: [u16; 5] = [28160, 32120, 32677, 32768, 0];
pub const DELTA_LF_MULTI: [[u16; 5]; 4] = [DELTA_Q; 4]; // same value x FRAME_LF_COUNT
```
(`FRAME_LF_COUNT = 4`, `MAX_LOOP_FILTER = 63`, `DELTA_Q_SMALL = DELTA_LF_SMALL = 3`.)

### Read logic (decodemv.c:85-146, `read_delta_qindex`/`read_delta_lflevel`,
called from `read_delta_q_params` at :732)
Both follow the same shape: a 4-symbol CDF read; if the result is 3
(`DELTA_*_SMALL`), extend with `L(3)`-then-`1+that`-many literal bits
(`rem_bits = literal(3)+1; thr = (1<<rem_bits)+1; abs = literal(rem_bits) +
thr`); if `abs != 0` read one sign literal bit (sign=1/no-read implied
"positive" when abs==0, matching spec/libaom exactly -- `sign = abs ?
read_bit() : 1`); `reduced = sign ? -abs : abs`. For q: `CurrentQIndex =
clamp(CurrentQIndex + reduced*delta_q_res, 1, 255)`. For lf: same shape,
`clamp(DeltaLF[i] + reduced*delta_lf_res, -63, 63)`, looped over 4 planes
if `delta_lf_multi` else just index 0 against the single `delta_lf_cdf`.

### The gating condition (decodemv.c:94-95, :121-122) -- **not** a spec
`ReadDeltas` global, libaom computes it locally per call:
```c
read_delta_q_flag = (mi_col & (mib_size-1) == 0 && mi_row & (mib_size-1) == 0);
if ((bsize != sb_size || !skip) && read_delta_q_flag) { ...read... }
```
Both conditions collapse to: **the very first block decoded inside a
superblock always has this MI position** (partition trees decode top-left
leaf first, always), so a per-SB `Cell<bool>` reset exactly like
`CDEF_TRANSMITTED` (decode.rs ~82, reset alongside it at the top of both
tile loops' `sb_c` loop bodies) replaces the position check with no
behavioural difference. The remaining condition, "skip the read entirely
(state carries over unchanged)", fires only when this first block **is
itself the whole superblock** (`bsize == sb_size`) **and** `skip == true`.

### Call sites -- only 4, not the dozen it looks like
`maybe_read_cdef_idx` (decode.rs:115) is called from exactly 4 places,
right after each one's own `skip` read, and delta_q/delta_lf's spec order
is `skip -> cdef -> delta_q -> delta_lf` (already commented at decode.rs
~2474: "spec order... `cdef` lands right here" -- delta lands right after
that same line, one function per call site):
- `read_intra_mode_rect` (decode.rs:1987, feeds `decode_block_rect`) --
  `bw`/`bh` never both 64 here (SB-level partition never recurses to a rect
  strip directly), so `is_whole_sb` is always `false`.
- `read_intra_mode` (decode.rs:2447, feeds `decode_block`/`decode_leaf8`) --
  has a `side: usize` param; `is_whole_sb = side == 64`.
- `decode_inter_block` (decode.rs:6344) -- has `side: usize`; same rule.
- `decode_inter_block8` (decode.rs:8215) -- always sub-8x8, `is_whole_sb`
  always `false`.

### Threading `CurrentQIndex` into dequant -- narrower than it looks
`base_q_idx: u8` is already threaded as a plain pass-through parameter
through every one of these functions down to exactly 2 real use sites
(confirmed by grep -- `base_q_idx` appears 92x in decode.rs but is *read*,
not just passed, only at decode.rs:2986 and :5397, both
`dequant_and_inverse_typed(..., i32::from(base_q_idx), ...)`). This means
the running quantizer state does **not** need a new parameter threaded
through every block/leaf/rect function signature: a `thread_local!
static CURRENT_Q_IDX: Cell<i32>`, reset to `base_q_idx as i32` at the top
of both `decode_key_frame_tile_with_cdfs`/`decode_inter_frame_tile_with_cdfs`
(same place `CDEF_BITS`/`CDEF_SB_COLS` already get set per tile), mutated
only inside the new `maybe_read_delta_q_lf` helper, and read back at the 2
dequant call sites instead of `i32::from(base_q_idx)`, reproduces the exact
running-state semantics with no signature changes to the block readers.
`DeltaLF` needs the same `Cell<[i32; 4]>` treatment, but **does** need a
consumer: `lf_level`/`edge_params` (decode.rs:3991/4020) currently read
only the frame-level `LoopFilterParams.level` array -- applying `DeltaLF`
there needs `MiGrid`/`fill_lf_grid` (decode.rs:1444) to carry a per-block
delta value forward to the deblocker, which is genuinely new state, not a
narrow substitution like q_idx. This is the part of Job 2 that needs a
real design pass, not just wiring -- flag it to whoever picks this up next.

### Header threading + refusal removal
`header.delta.{q_present,q_res,lf_present,lf_res,lf_multi}`
(`ec_av1_syntax::frame::DeltaParams`, already parsed) need to become new
parameters on `decode_key_frame_tile_with_cdfs`/`_inter_..._with_cdfs` (and
their public non-`_with_cdfs` wrappers, and `stream.rs`'s 2 call sites),
mirroring how `CdefParams`/`LoopFilterParams` are already threaded. Then
delete the `stream.rs:170-174` refusal
(`"a frame with delta_q_present or delta_lf_present set..."`).

### Gate
Reproducible refusal fixture already on file (r2's note): `--aq-mode=1` or
default `cpu-used>=1` at 128x64+. Same shape as the CDEF gate (128x64,
2 SBs, `--threads=1 --row-mt=0`), with a hard-asserted firing counter
(`DELTA_Q_HITS`/`DELTA_LF_HITS`, same `Cell<usize>` pattern as
`CDEF_IDX_HITS`).

## Remaining refusal strings verbatim (all pre-existing, untouched by this
lane except the one new one above)
- `"a partition type this encoder never writes"` (decode.rs, intra SB
  level, key frame catchall) -- pre-existing, real, out of scope.
- `"a partition type this encoder never writes (value={part32})"` -- pre-existing.
- `"an INTER 32x32 partition type this encoder never writes (value={part32})"` -- pre-existing.
- `"a frame with delta_q_present or delta_lf_present set (this decoder
  never reads the per-superblock delta symbols)"` -- Job 2's target,
  **still present, still refuses correctly** (not touched this round).
- `"an inter SB-level partition type other than SPLIT (this decoder's
  inter tile path only recurses a superblock as SPLIT)"` -- **new this
  lane**, Job 1.

## Merge note
`gate_coverage.rs`/`refusal_inventory.rs` (main-only guards this worktree
doesn't have) need the one new refusal string above added at merge; nothing
to delete from `gate_coverage.rs` this round since no gate closed a tool
entry (Job 1 has no gate by design, see above).

## Next lever
Job 2 exactly as scoped above. The `DeltaLF`-into-deblocker sub-problem
(new `MiGrid` state, not a narrow substitution) is the one piece that needs
design before code -- everything else (CDFs, read logic, call sites,
`CURRENT_Q_IDX` substitution, header threading, refusal removal, gate
recipe) is now a direct port, not an investigation.
