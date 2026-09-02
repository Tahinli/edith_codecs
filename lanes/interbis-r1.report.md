# lane-interbis r1 — the "decodes fully, still mismatches" inter defect

Branch `lane-interbis`, worktree `edith_codecs-interbis`, off lane-inter4 `d2d7327`.
No rebase (main unchanged since that base).

## STEP 1 — reproduced and pinned

The rect gate's 8-bit **attempt 1** is the one that decodes fully and mismatches:
192x128, 6 frames @25, `geq=lum='128+58*sin((X+N*3)/6)+18*sin(Y/23)'`, cq 34,
`--enable-tx-size-search=1`, `--min/max-partition-size=16/32`, cpu-used=0, lag 0.
Regenerated twice, byte-identical:
`fa70b7f68d311e577e3d14a86574909e6e58cac817b5a7fae04341c41ff50260`
(script `$HOME/.cache/interbis/gen.sh`; 10-bit twin
`67cad9bd5341db897de8a5308faecaedb1b0df761e4f315f1817156d124caef7`).

Decode order == display order (no hidden frames; `--lag-in-frames=0`).
Frames 0..4 exact; **frame 5** luma only, 646 px, bbox x160-191 y93-127, max delta 39.
`EC_AV1_PREFILT_DUMP` (ours) vs the same rung from the instrumented aomdec:
frames 0-4 byte-identical, frame 5 differs in 640 luma px at x160-191 y96-127 —
i.e. **reconstruction**, not deblock/CDEF/LR, and exactly one 32x32 block:
mi (row 24, col 40), the frame's last block.

EVIDENCE: $HOME/.cache/interbis/{o_pre.f5,a_pre.f5} | EC_AV1_PREFILT_DUMP ours vs aomdec, per-frame byte diff | frames 0-4 identical, frame 5 640 luma px wrong in the single 32x32 block at mi(24,40)

## STEP 2 — first divergent element

Entropy is **exact**: the msac range ladder agrees at that block's entry
(`EC_MODE ... rng=37852` both) and at the end of its mode read
(`rng=59818` both). Both decoders read mode 13 (NEARESTMV), ref0=2 (LAST2),
stack size 3. The **VALUE** differs:

    ours   EC_MODE_VAL mi_row=24 mi_col=40 mode=13 ref0=2 mv0=(0,72)
    aomdec EC_MODE_VAL mi_row=24 mi_col=40 mode=13 ref0=2 mv0=(0,-230)

New `EC_STACK` rung (added to both decoders) shows entries 1 and 2 identical
(`(0,24) w=2`, `(0,-785) w=2`, the extra-search neighbours) and entry 0 —
the temporal (MFMV) candidate — wrong: ours `(0,72) w=32`, aomdec `(0,-230) w=16`.

New `EC_TPL` rung (per `add_tpl_ref_mv` probe, both decoders) isolates it to the
projected motion field itself: over the 16 probes of that block, aomdec reads
`INVALID` at blk_col 0/2 and `mfmv0=(0,-230) rfo=3` at blk_col 4/6; ours reads
`mfmv0=(0,24) rfo=1` at all 16. `(0,24)*3/1 = (0,72)` — our wrong stored pair
projected exactly to the wrong candidate.

EVIDENCE: $HOME/.cache/interbis/{o_tpl.txt,a_tpl.txt} | EC_TRACE_MODE + EC_TRACE_TPL on both decoders, block mi(24,40) of frame 5 | ours mfmv0=(0,24) rfo=1 x16 vs aomdec INVALID x8 + (0,-230) rfo=3 x8; stack entry 0 (0,72) vs (0,-230)

## Root cause (decode.rs `build_motion_field`, spec 7.19 / libaom `av1_copy_frame_mvs`)

Our per-frame saved motion field stored **`ref_frame[0]`/`mv[0]` of every inter
block**. libaom walks **both** reference slots and the **last qualifying one
wins**, skipping a slot when

* `cm->ref_frame_side[ref]` is non-zero — the reference's OrderHint is ahead of
  (or equal to) this frame's (`av1_calculate_ref_frame_side`), or
* `|mv| > REFMVS_LIMIT` (`(1<<12)-1`).

Every compound block in this stream is `LAST + GOLDEN` (both past), so libaom
stores GOLDEN's mv with `ref_frame_offset = 3` where we stored LAST's with
offset 1 — a wrong (mv, distance) pair in every compound block's 8x8 cell, which
the next frame's temporal candidate then projects with the wrong reference
distance. Class name: **saved-field-keeps-slot-0** — a per-frame/DPB *summary*
of a multi-slot field must replay the reference's own slot walk (order + skips),
not keep slot 0. (Sibling of `neighbour-votes-all-its-fields`: there the
consumer voted only one slot, here the producer stores only one.)

Fix: `crates/ec-av1/src/decode.rs:1728-1800` (`build_motion_field` now takes
`order_hint_bits`, walks both slots last-wins, applies the side and
`REFMVS_LIMIT` skips); call site `decode.rs:19040`.

## Sweep

`build_motion_field` is the only writer of `MotionField`/`SavedMv`
(`grep -rn "SavedMv\|MotionField" crates/ec-av1/src`); the mv-stack neighbour
maps already carry both slots (`MiInfo::ref_frame1`/`mv1`), and
`motion_field_projection`/`setup_motion_field` were re-read line-for-line
against `mvref_common.c` this round and match (one known, unreached gap noted
below).

## Gate — GREEN, and it binds

`a_real_aomenc_inter_sequence_with_temporal_mv_candidates_decodes_pixel_exact`
(`crates/ec-av1/src/stream.rs`, next to the rect gate): the pinned recipe above,
2 tx-size-search arms x {8-bit, 10-bit}, every fully decoded attempt compared
Y/U/V against ffmpeg, refusals counted (never SKIPped), and `decode::tmv_hits()`
asserted > 0 per depth.

    cargo test -p ec-av1 --lib temporal_mv_candidates -- --nocapture
    8-bit:  1 named refusal, 1 pixel-exact attempt, temporal-MV blocks = 66
    10-bit: 1 named refusal, 1 pixel-exact attempt, temporal-MV blocks = 67
    test result: ok. 1 passed; 0 failed

EVIDENCE: $HOME/.cache/interbis-tmvgate.log | cargo test -p ec-av1 --lib temporal_mv_candidates | 8-bit + 10-bit pixel-exact, tmv counter 66/67

Negative control (gate binds): with the store put back to first-slot-wins the
same gate fails at `frame 5 luma vs ffmpeg (attempt 1, 8-bit)`.

EVIDENCE: $HOME/.cache/interbis-tmvgate-neg.log | one-line revert to `if saved.is_none()` then re-run the gate | FAILED, frame 5 luma, restored immediately after

## Instruments added (env-gated, both decoders)

- ours: `EC_MODE_VAL` + `EC_STACK` on the single-ref path of `decode_inter_block`
  (byte-format-identical to the oracle's rung), `EC_TPL` per `add_tpl_ref_mv` probe.
- oracle (`~/.cache/aom-oracle/src`): `EC_STACK` (stack dump at `EC_MODE_VAL`) and
  `EC_TPL` (per probe, incl. the `INVALID_MV` arm); `ninja aomdec` rebuilt.

## Residue

- **accepted: `motion_field_projection` does not check the source frame's
  frame_type.** libaom returns 0 for a KEY/INTRA_ONLY source; here an intra
  frame's saved field has no cells set, so the projection is inert — but its
  *return value* (which feeds `ref_stamp`) can differ. Unreached on every stream
  swept; fix-now if a future stream shows a `ref_stamp` divergence.
- **deferred: the rect inter residual gate** stays `#[ignore]`d (lane-inter4's
  own residue, unblocked by 16x8/8x16 rect inter leaves); its
  `out_of_scope_mismatch` assert is what surfaced this defect and is now
  satisfiable on the 8-bit arm.
- **deferred: film probe** — this diff lifts no refusal; both films still stop at
  lane-rectchroma's intra-rect-strip refusal.

## Suite

    $HOME/.cache/interbis-suite.log (systemd-run unit, MemoryMax=10G)
    test result: ok. 324 passed; 0 failed; 31 ignored; finished in 443.08s

323 (lane-inter4 r2 baseline) + this round's new gate. Siblings inside that run:
inter_sequence*, obmc, warp, globalmv, compound_*, altref/hidden-frame,
tx_select, refusal_inventory, gate_coverage — all green.

EVIDENCE: $HOME/.cache/interbis-suite.log | ec-av1 --lib at 0527911 | 324 passed / 0 failed / 31 ignored
