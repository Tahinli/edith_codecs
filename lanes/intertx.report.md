# lane-intertx — the inter tx-type search on film (chroma inheritance made honest)

Base: `810d8972` (charter). Fix commit: `9af60b4d`. Regression fix + report
commit: this one. Disposition: **stays-off**
(`crate::speed::TX_TYPE_SEARCH_INTER` keeps `Screen` at presets 0-6; the keep
rule's deciding column fails at best -0.1 against the required -0.5), but the
lane's correctness work is real and committed: the chroma inheritance desync
was LIVE and is fixed, the wider inter search is reachable and honest on
`reduced_tx_set = 0` frames, and on screen content the fix itself measures
-0.7/-0.4 BD against the pre-lane standing table. Pins untouched at
8291 / 33227 (default non-screen streams byte-identical; pin test re-run
green). Never merged, never pushed.

## 1. The chroma inheritance desync: LIVE, fixed

The wider inter search (`inter_tx_type_candidates`' seven-type `INTER_WIDE`
list) was unreachable: `inter_luma_set` named only the plain `Luma*Inter`
sets, so the `Set1` arms were dead code. The refusal reason lane-txi recorded
(the refs lane's DPB bookkeeping) was already known wrong; the real blocker
was `refusal-from-own-desync` in the CHROMA inheritance:

- (a) the encoder never set `FrameCtx::reduced_tx_set_inter` on the shared
  frame context, so `recode_inter_chroma`'s
  `reduce_inherited_chroma_tx_type` reduced against a STALE allowance —
  whatever the previous frame's own trial decode had last left in the cell —
  while a `reduced_tx_set = 0` frame's decoder allows the full set;
- (b) `tile::write_block_planes` coded every chroma plane as `DCT_DCT`.
  Chroma codes no `tx_type` symbol, but the type names the transform class,
  and the class picks the scan, the `eob_pt` row and the neighbour contexts
  (`write_coeffs`' lane-txw note) — so an inherited 1-D type desynced the
  chroma exactly as luma once did.

Fix (commit `9af60b4d`): (a) `encode_inter_frame` sets
`fctx.reduced_tx_set_inter` from the frame's own header bit the moment it is
decided (`header.reduced_tx_set = !wide_tx_set(screen)`); (b) the writer
derives each inter block's chroma type from the first luma unit's coded type
via the new `decode::reduce_inherited_chroma_tx_type_flagged` against
`Cdfs::reduced_tx_set` — the frame's own honest bit — and codes chroma with
it; the search names the Set1 sets through `luma_set_for` (the intra search's
own shape) at both the flat and the split-unit trial, so every candidate is
priced in the alphabet its symbol is coded into. All six
`write_block_planes` callers now pass the block's inter flag; intra chroma
rides `DCT_DCT`'s 2D class exactly as before.

Witness `encoder::tests::a_non_screen_inter_clip_codes_a_one_d_type_both_decoders_read_every_plane_exactly`
(process-global `set_inter_tx_search(Some(true))` + `set_wide_tx_set(Some(true))`
+ `force_screen(Some(false))` under `knob_write`, 320x192 moving
steps/ramps/plateaus, gop 4, q 90): the inter search WON and CODED
non-`DCT_DCT` types on the non-screen clip, and ALL THREE planes read
sample-exact through `decode_stream` AND ffmpeg at every frame.

EVIDENCE: ~/.cache/intertx/witness-green.log | --ignored --exact --nocapture
run of the witness on `9af60b4d` | "inter wide-set witness: inter hits
[200, 979, 3, 97, 111, 40, 521, 0, ...], 8143 bytes" — coded>0 guards pass
for `IDTX` (200) and for the 1-D types (`V_DCT`=40 + `H_DCT`=521); all three
planes identical to ffmpeg on all four frames. ok.

RED run (desync live): with the widening applied and fix (b) neutralized —
chroma coded `DCT_DCT` unconditionally, the pre-lane writer — the same encode
FAILS inside its own trial decode: the reader inherits the 1-D chroma type
the writer refused to code with, the tile desyncs, and the derail surfaces as
"a reference frame selected with no picture at this frame's own
ref_frame_idx slot" (class `refusal-from-own-desync`, not a real ref defect).

EVIDENCE: ~/.cache/intertx/witness-red.log | same witness, fix (b) probe
removed | FAILED: trial-decode refusal aborts the encode at flush.

Standing witnesses re-run green on the fixed HEAD: the wide witness now codes
1-D inter units on its screen card too (`V_DCT`=41, `H_DCT`=405, `IDTX`=702
of 1305) and reads every frame exact; the two-type inter witness passes.

EVIDENCE: ~/.cache/intertx/standing-witnesses.log | --ignored --exact and
default runs | "a_wide_tx_set_clip... ok", "an_inter_clip... ok".

## 2. Long-GOP BD (deciding; 48 pictures, gop=48, both films, VPS-1)

Control = default (lever `Screen`), arm = `EC_AV1_TXSET_INTER=all`, same HEAD
`9af60b4d`, same ladder (`encode::tests::bd_rate_film_long_gop`,
`--ignored --exact`). BD-rate, lower better:

| row | ctrl vs libaom | ctrl vs rav1e | arm vs libaom | arm vs rav1e |
|---|---|---|---|---|
| film A (1080p) 1920x768 | +20.9% | -9.0% | +20.8% | -9.1% |
| film B (2160p HDR) 1920x1024 | +70.4% | -2.2% | +70.3% | -2.1% |

Arm-minus-control: film A -0.1/-0.1, film B -0.1/+0.1. The control
reproduces the standing table (b128res-era film A +20.9/-9.0, film B
+70.4/-2.2) to the digit on a different HEAD.

Wall ours: film A 1314.7s -> 1395.1s (+6.1%), film B 1031.5s -> 1074.0s
(+4.1%). LOADAVG 0.28/0.35/0.20 before, 1.30/1.20/1.11 after (control);
0.93/1.12/1.09 before, 1.12/1.10/1.09 after (arm).

EVIDENCE: ~/.cache/intertx/longgop-control.log | gate 2 control, VPS-1,
MemoryMax=5G, `systemd-run --user` | film A +20.9%/-9.0%, film B
+70.4%/-2.2%; census 100.0% `DCT_DCT` on both films. ok.
EVIDENCE: ~/.cache/intertx/longgop-arm.log | gate 2 arm, EC_AV1_TXSET_INTER=all |
film A +20.8%/-9.1%, film B +70.3%/-2.1%. ok.

## 3. 12-frame native guard (all five rows, VPS-1)

`encode::tests::bd_rate_screen_native`, control and arm:

| row | ctrl vs libaom | ctrl vs rav1e | arm vs libaom | arm vs rav1e |
|---|---|---|---|---|
| bars 1080p | -3.3% | -19.0% | -3.5% | -19.0% |
| bars 2160p | +8.4% | -13.9% | +8.5% | -13.8% |
| film A | +17.9% | -6.3% | +17.8% | -6.4% |
| film B | +22.6% | -3.8% | +22.7% | -3.7% |
| screen capture | +13.7% | -33.6% | +13.7% | -33.6% |

The screen row is byte-identical between control and arm (same PSNR/bytes at
every point, same census) — exactly the charter's prediction: the search is
already on for screen frames, so `All` can add nothing there.

Wall ours: bars 1080p 396.2s -> 411.1s (+3.8%), bars 2160p 336.3s -> 358.1s
(+6.5%), film A 350.9s -> 371.0s (+5.7%), film B 301.4s -> 309.8s (+2.8%),
screen capture 226.1s -> 225.1s (-0.4%). LOADAVG 0.88/1.05/1.07 before,
1.07/1.14/1.11 after (control); 0.91/1.10/1.10 before, 1.12/1.13/1.09 after
(arm).

Screen-content attribution note: the control screen row against the
b128res-era standing table moved +14.4% -> +13.7% (libaom) and -33.2% ->
-33.6% (rav1e). That delta is THIS lane's fix reaching the screen default:
the widened inter candidates are now offered to screen frames at preset 0,
where the lever already said `Screen`, and the census shows the new types
coding (below). Cross-HEAD comparison, so attribution is indicative only.

EVIDENCE: ~/.cache/intertx/native-control.log | gate 3 control, five rows |
table above, control columns; three-way exactness held at every clip x q x
frame. ok.
EVIDENCE: ~/.cache/intertx/native-arm.log | gate 3 arm, EC_AV1_TXSET_INTER=all |
table above, arm columns. ok.

## 4. Inter tx-type census (search won / coded, per clip)

Non-`DCT_DCT` share of COMMITTED inter luma units (the census names types
that were really coded), arm runs:

| clip | inter units | coded non-DCT_DCT | share |
|---|---|---|---|
| film A long-GOP | 1468708 | 35618 `IDTX` | 2.4% |
| film B long-GOP | 1250132 | 41586 `IDTX` | 3.3% |
| bars 1080p 12f | 115907 | 21033 `IDTX` | 18.1% |
| bars 2160p 12f | 83614 | 20634 `IDTX` | 24.7% |
| film A 12f | 330189 | 7311 `IDTX` | 2.2% |
| film B 12f | 310009 | 7002 `IDTX` | 2.3% |
| screen capture 12f | 59731 | 1080 `IDTX` + 266 `ADST_ADST` + 346 `ADST_DCT` + 187 `DCT_ADST` + 455 `V_DCT` + 1572 `H_DCT` | 6.5% |

Controls: 100.0% `DCT_DCT` on every non-screen clip (lever off there);
screen capture control already carries the wider alphabet (the fix's doing).
With `all`, film takes only the two-type set's `IDTX` (2.2-3.3%) — the
arm is INERT ON FILM, the same 0-to-small band the b128res arm showed.
Every one of these units decoded three-way exact (encoder == ffmpeg ==
decode_stream) at every q point of every run — the gates' standing
exactness assertion covers the whole census, native resolution, both films
and all five native rows.

## 5. Keep rule — NOT met

One film row >=0.5 BD down on a column: best delta anywhere is -0.1 (film A
long-GOP and 12-frame; native bars rows are not film). Other film row flat
within +/-0.3: yes (max 0.1). Screen not worse by 0.3: yes (byte-identical).
Wall <= +15%: yes (max +6.5%). The deciding column fails at -0.1 against the
required -0.5: `IDTX` at 2-3% of film units buys a tenth of a point. The
lever stays `Screen` at every preset; `speed.rs` is untouched.

## 6. Pins, witnesses, scope

- Pins `encode::tests::the_encoders_own_streams_are_byte_identical_to_their_pins`:
  re-run green on `9af60b4d` — `(150, 8291, 0x1f00bb0eb099a27f)` and
  `(60, 33227, 0x57ee6b1f8eacd881)` stand. The pin fixture is the non-screen
  h264 clip, where the fix changes nothing by construction (the widened
  candidates need `reduced_tx_set = 0`, which only screen frames code at the
  default preset). No pin re-take.

EVIDENCE: ~/.cache/intertx/pins.log | default-preset pin run on 9af60b4d |
ok, both pins byte-identical.

- VPS-1 provisioning verified before the first gate: filmA-gate.mkv 8833494
  bytes, filmB-gate.mkv 195927872 bytes, no partial files.
- Gate recipe deviations from charter §3 (all recorded): `systemd-run`
  needs `-p WorkingDirectory=` (the unit does not inherit the shell's cwd),
  `PATH`/`CARGO_TARGET_DIR` must be set INSIDE the unit (a
  `bash -lc` wrapper), and the synced repo's `.git` was a stale worktree
  pointer file that broke the memguard runner's `git rev-parse` — replaced
  with a fresh `git init` on the VPS. One gate at a time throughout.
- Not verified: nothing in the charter's scope is unverified. The
  three-lane lib suite and `cargo check --workspace --all-targets` were NOT
  run — charter §2 requires them only on the keep-rule-met (ships-on)
  branch; the stays-off branch carries the scoped local checks (ec-av1 lib
  + tests compile warning-free; the three tx witnesses; the pin test) and
  project-wide validation stays with the main agent. `FLIPADST` family and
  the 1-D ADSTs remain unoffered (the standing lane-txi deferral).

## 7. Regression caught by the merge tier: the raw-tile decode wrapper's
## stale `reduced_tx_set`

Main's regression tier caught three `decode::tests` failures on the lane
HEAD (`a_gop_round_trips_bit_exact_against_the_encoder_reconstruction`,
`an_odd_size_gop_round_trips_bit_exact_against_the_encoder_reconstruction`,
`ffmpeg_and_this_decoder_agree_on_a_gop`), all refusing with "a reference
frame selected with no picture at this frame's own ref_frame_idx slot".

Root cause (class: **test asserts against a stale header** -- the wrapper's
own parameter docs document five prior instances of exactly this class):
`decode::decode_inter_frame_tile_lr`, the raw-tile decode helper the GOP
round-trip tests use, hard-coded `reduced_tx_set: true`
(`decode.rs:34552` at the fix commit's parent) with the comment "this
wrapper's callers are the decoder's own tests, which all code the reduced
sets". True until this lane: no inter block ever inherited a non-`DCT_DCT`
chroma type. With the widened inter search live, a screen-detected GOP at
the default preset codes `reduced_tx_set = 0` frames whose inter luma takes
`V_DCT` (the traces show the first inter block at (0,0) 16x16 winning
exactly that), and the chroma inheritance reduces the inherited type
against the very bit the wrapper guessed: the test's decode derived
`DCT_DCT` from `inh=Some(V_DCT)` at chroma 8x8 where the writer coded
`V_DCT`, desynced, and the garbage symbols eventually read a ref_frame_idx
that names an empty slot -- the named refusal is downstream noise, the
first divergence is the chroma type.

Fix (this commit): the wrapper takes the frame's own
`reduced_tx_set: bool` (placed before `tx_select`, mirroring the plural
fn), `Encoded` carries the bit (`encode.rs`, the `screen`/`tx_select`
pattern), the GOP tests and the key-frame tile test (`stream.rs`, whose
"our own encoder always writes `reduced_tx_set: true`" comment had the
same latent stale-header bug) pass the honest value.

Why the gates and witnesses did not see it: every production decode path
(`stream.rs`'s parser, `decode_stream`, ffmpeg) reads the bit off the wire
honestly -- the gates' three-way exactness and all witnesses ride those.
Only the raw-tile test helper guessed.

Encoder output is UNCHANGED by the fix: the diff touches the `Encoded`
metadata struct, the test-facing decode wrappers and one test call -- no
writer or search code (tile.rs is not in the fix diff at all), and the
three witnesses code byte-identical streams after it (8143 / 8211 bytes,
same census). The VPS-1 BD tables in sections 2-4 remain valid; no gate
re-run needed.

EVIDENCE: ~/.cache/intertx/regression-repro.log | the three tests on the
unfixed lane HEAD | 3 failed, all with the named refusal.
EVIDENCE: ~/.cache/intertx/regression-fix.log | the three tests + the pin
test after the fix | 4 passed.
EVIDENCE: ~/.cache/intertx/witness-rerun.log | all three tx witnesses
re-run after the fix | byte-identical streams (8143 / 8211 bytes), ok.

## 8. Full suite and workspace check (post-fix)

Three-lane lib suite LOCALLY on the fix commit, `--release --lib`:
`--skip stream::` 348 passed / 0 failed; `stream:: --skip 10bit` 202
passed / 0 failed; `10bit` 42 passed / 0 failed -- **592 passed, 0
failed**. `timeout 900 cargo check --workspace --all-targets -j4`: 0
ec-av1 warnings (the 25 workspace warnings are ec-opus's, pre-existing).

EVIDENCE: ~/.cache/intertx/suite-lane1.log | --skip stream:: | 348 passed.
EVIDENCE: ~/.cache/intertx/suite-lane2.log | stream:: --skip 10bit | 202 passed.
EVIDENCE: ~/.cache/intertx/suite-lane3.log | 10bit | 42 passed.
