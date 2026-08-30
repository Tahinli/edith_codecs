# lane-realworld r6 report

VERDICT: Job 1 (gate delta_q/delta_lf before merge) DONE, committed
(6981ef5). Both halves are now gate-proven against a real aomenc stream,
not just code-verified against the spec's shape. "Then" item
(`--enable-intrabc`) declined this round -- see below, lane says clear.

## Job 1 -- the gate (6981ef5)

r5 warned its own removal of the whole-frame `delta_q_present ||
delta_lf_present` refusal was unproven: no fixture anywhere set
`delta_lf_present`. Verified against libaom source before writing any
encoder flags, per the charter's own instruction not to assume:

- `--deltaq-mode` help text says "requires --enable-tpl-model=1" but
  `enable_tpl_model` defaults to `1` (`av1_cx_iface.c:234`), so nothing
  extra was needed there.
- The **default** `--deltaq-mode=1` (`DELTA_Q_OBJECTIVE`) additionally
  gates `delta_q_present_flag` on `gf_group->update_type == LF_UPDATE`
  and `allow_deltaq_mode(cpi)`'s own RD search returning a negative
  cost sum across every superblock (`encodeframe.c:2192-2211`) -- an
  extra, content-dependent condition worth avoiding for a gate that
  needs to fire reliably.
- `--deltaq-mode=2` (`DELTA_Q_PERCEPTUAL`) skips that branch entirely:
  `delta_q_present_flag = deltaq_mode != NO_DELTA_Q`, gated only by
  `base_qindex > 0` (guaranteed by `--cq-level=45`, nonzero). Used this
  instead of the charter's suggested `--deltaq-mode=1`/`--aq-mode=1`.
- `--delta-lf-mode=1` sets `extra_cfg->deltalf_mode`, which
  `tool_cfg->enable_deltalf_mode = (deltaq_mode != NO_DELTA_Q) &&
  extra_cfg->deltalf_mode` (`av1_cx_iface.c:1269-1270`) ANDs onto
  `delta_lf_present_flag = delta_q_present_flag &&
  enable_deltalf_mode` (`encodeframe.c:2217-2218`). This flag is real
  and does exactly what the charter asked to confirm: r5's worry that
  `--deltaq-mode` alone might never set `delta_lf_present` was correct
  -- `--delta-lf-mode=1` is the separate flag needed.
- 128x64 (2 superblocks; this decoder hardcodes 64px SBs) is the
  charter's own minimum for a per-superblock symbol to have anywhere to
  differ.
- `--cpu-used=4` (not `=0`, which every other multi-SB gate here avoids
  per the cdef gate's own documented dead-end: aomenc's RD at
  `--cpu-used=0` picks HORZ_4/VERT_B at part64 regardless of the
  rect/ab/1to4-partition flags, and this decoder's intra part64 match
  only covers NONE/SPLIT). Reused the cdef gate's exact working
  toolset otherwise (same `--enable-*` flags, same mandelbrot-source
  seed sweep for RD diversity).

New test `a_real_aomenc_stream_with_delta_q_and_delta_lf_decodes_pixel_exact`
(`crates/ec-av1/src/stream.rs`, appended at EOF): 40/40 attempts
pixel-exact vs `ffmpeg`, `delta_q_hits()=1920`, `delta_lf_hits()=1920` at
the end of the run, both hard-asserted `> 0` (not soft-skipped --
r5's `--deltaq-mode`/`--delta-lf-mode` combination fires reliably on
every attempt, unlike the wedge-interintra gate's occasional zero-hit
run). Also asserts any `"unsupported"` refusal does not name
`delta_q`/`delta_lf` -- that capability is ported, a refusal naming it
would be a regression.

`EC_AV1_REQUIRE_AOMENC=1 cargo test -p ec-av1 --lib -j4`: **243 passed,
0 failed, 17 ignored** (r5's 242 plus this one new gate).

### One documented gap, not gate-provable

libaom hardcodes `delta_q_info.delta_lf_multi = DEFAULT_DELTA_LF_MULTI`
(`enums.h:73`, value `0`) in the encoder's own frame-setup path with **no
CLI flag that ever sets it to 1** (confirmed by grepping every
`delta_lf_multi =` assignment in `av1/encoder/`: only the one hardcoded
default). This gate therefore proves the single-plane
`!delta_lf_multi` branch of `maybe_read_delta_lf` (the branch every real
aomenc stream can ever produce) but cannot and does not claim to
exercise the 4-plane `delta_lf_multi` loop -- that path stays
code-verified against the spec's read shape only, same disposition r5
left the whole feature in, now narrowed to just this one sub-branch.
Not a refusal (the code correctly handles `delta_lf_multi=false`, which
is the only value a real encoder ever writes), just a residual
proof-gap worth naming per the charter's own standard.

## Refusal strings changed this lane

None. The whole-frame refusal was already removed by r5; this round
added a gate, not a code change to `decode.rs` or
`refusal_inventory.rs`.

## "Then" -- `--enable-intrabc`

Declined this round; lane says clear. `decode.rs:2767-2786` already
documents why in its own comment (lane-screen scope): actual intra
block copy needs `assign_dv`'s DV-prediction machinery this decoder
does not carry (`av1_find_mv_refs`/`av1_find_ref_dv` equivalents --
DV-specific validity checks against tile/frame boundaries, a DV
predictor stack distinct from the existing NEWMV mv-stack, then
integer-pel same-frame block-copy prediction with decoded-region
bounds checks). Every comparable feature already landed in this repo
(warp, OBMC, interintra, masked compound, wedge) took multiple
dedicated lane-rounds each per the ledger; attempting a full IntraBC
landing in whatever budget remained after Job 1 risked either a
shallow/wrong port (a new desync class, per this repo's own extensive
history of exactly that) or leaving genuinely unfinished work
uncommitted at the turn cap. Job 1 was the charter's stated priority
("nothing else in this lane matters until it is done"); IntraBC is
sized for its own dedicated lane, not a "then, if turns remain" addendum.

## Merge note

No refusal strings added/removed/reworded. `gate_coverage.rs`'s
`NEVER_EXERCISED` derivation only tracks `--enable-*` flags (verified by
reading its parser); `--deltaq-mode=`/`--delta-lf-mode=` are not
`--enable-*` flags so this new gate needs no entry there, and
`gate_coverage.rs` itself is unmodified. `crates/ec-av1/src/stream.rs`
is the only file touched this round.
