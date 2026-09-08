# lane-golomb2 — the straddle gate's fixtures, and the Golomb bound

Two tasks. (A) landed a fix-equivalent gate; (B) is a REFUTED premise plus a
found defect that is not the chartered one.

## (A) the straddle gate's SEG_LVL_ALT_Q rows are a recipe now

`stream::tests::a_straddling_frame_decodes_exactly_on_both_reference_encoders_ladders`
used to pin two crops of a real film (`fixtures/d792_straddle_rav1e_384x152_
q{100,150}.obu`). `fixtures/` is gitignored, the files could never be
committed, and the test `continue`d when they were missing, so the only half
of the gate that saw the rect-dequant defect was blind on every fresh clone
(class `gate-skips-on-its-own-failure`).

**Recipe** (in the test, no new script): two lavfi inputs hstacked --
`color=c=gray:s={w/2}x{h}` and the same with `noise=alls=80:all_seed=7:allf=
t+u` -- encoded by `ffmpeg -c:v libaom-av1 -cpu-used 6 -b:v 0 -crf {20,35}
-aq-mode 1 -g 4 -threads 1` at 384x152 and 384x184, 4 frames, decoded by
ffmpeg and by us and compared on all three planes. The flat/noise contrast is
what turns libaom's activity segmentation on; a `testsrc2` or repo-clip source
codes ONE segment (ledger dead-end) and would pass with the defect in.

**The property is asserted, not hoped for**: new `decode::block_q_span()` (a
`hit_do!` counter on `block_q_idx`, so it compiles out without
`gate-counters`) reports the lowest and highest quantizer index any block was
dequantized with, and each row asserts `hi > lo`. Measured: **q 33..106 at
crf 20, 89..160 at crf 35**, both sizes. `EC_DQCOEFF=1` on the same stream
prints `base=79 seg=0 q=33 / seg=5 q=98 / seg=6 q=106` -- three segments with
distinct `SEG_LVL_ALT_Q`.

**Sensitivity proof (measured, not asserted)**: the six sites of `e4cab216`
reverted by hand (`block_q_idx(fctx)` -> `current_q_idx`), gate re-run ->

```
a_straddling_frame...: segmented 384x152 crf 20, frame 0 plane U sample (160, 64):
ours 126 vs ffmpeg 123 (block q span 33..106)
```

i.e. the FIRST recipe row fails pre-fix. Restored, gate green: **20
straddling reference points sample-exact, 4.1 s** (4 recipe + 16 ladder).

**Deleted**: `fixtures/d792_straddle_rav1e_384x152_q100.obu` and `..._q150.obu`
from the shared fixtures dir; the only references (this test, and the "Pinned
rows" section of `lanes/d792.report.md`) are rewritten. The gate still SKIPs
with a printed reason when ffmpeg or libaom is absent.

## (B) the Golomb bound is libaom's own — REFUTED, no widening

The charter asked to widen `decode.rs read_golomb` "to the spec/libaom bound
(up to 32 leading zeros)". libaom does not have that bound:

```c
/* av1/decoder/decodetxb.c:22-43 */
static int read_golomb(MACROBLOCKD *xd, aom_reader *r) {
  ...
    if (length > 20) {
      aom_internal_error(xd->error_info, AOM_CODEC_CORRUPT_FRAME,
                         "Invalid length in read_golomb");
```

which is our `if length > 20` to the bit (ours starts `length = 1` and bumps
before the test, libaom starts 0 and bumps after: both error on the 21st zero
bit). Spec 5.11.40 BREAKS at `length == 20` and 5.11.39 masks the level with
`0xFFFFF`, and `refusal_inventory::read_golomb_reads_every_value_a_conformant_
stream_can_carry` already enumerates that whole value domain. So there is no
reader gap and nothing to lift: this refusal is always an earlier desync's
SYMPTOM (every historical instance closed that way -- lane-dgolomb's
`force_integer_mv`, lane-dkey's palette, lane-t900 r12's `uv_mode_grid`), and
raising the cap trades a clean refusal for silent corruption (lane-scaledref
r1 measured exactly that). The two doc comments that suggested otherwise are
corrected in place; the refusal stays in the inventory.

**The chartered refusal does not reproduce at `aba89feb`**: film A,
`crop=1920:792`, 12 frames, libaom `-cpu-used 6 -crf 5/20/35/45 -aq-mode 1`
and rav1e `speed=6 quantizer=50/100/150/200` -> **8/8 sample-exact vs ffmpeg**
(crf 5 included). No `EC_GOLOMB` line fired on any ladder stream in this lane.

## FOUND (not chartered, not fixed): libaom `-aq-mode 1` at 1920x1024 on film B

Same ladder at `crop=1920:1024` on film B: **rav1e 4/4 exact, all four libaom
points DIFFER** -- crf 45 diverges on the KEY frame at luma (883, 888), 188k
samples, growing to 530k by frame 10. Dropping `-aq-mode 1` from the same
recipe makes crf 45 **12/12 exact**, so it is the segmentation path again
(sibling of the (A) defect, class `parsed-then-discarded`). It does NOT
reproduce on 640x384 or 384x152 crops of the same source at the same recipe,
so a witness needs the large frame. Streams kept at
`$HOME/.cache/golomb2/streams/filmB_aom{5,20,35,45}.obu`.

`deferred: the film B -aq-mode 1 divergence — out of this lane's scope and no
small witness yet — unblocked by an aomdec EC_TRACE_COEFF/EC_IMODE diff at
mi(222, 220) of frame 0 of filmB_aom45.obu`
