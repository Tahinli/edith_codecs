# lane-lookahead — one mini-GOP of lookahead, and the ARF filter's future half

Worktree `edith_codecs-lookahead`, branch `lane-lookahead` off main `eb2af4ee`.
Deciding gate `encode::tests::bd_rate_film_long_gop` (48 pictures, both film
rows, BD vs libaom `cpu-used 6` / rav1e `speed 6`, lower is better).
**Control RUN, not quoted: film A +22.0 / −8.4, film B +76.5 / +1.2** — the
charter's +22.2/−8.2 and +77.5/+1.7 to within the gate's own noise.

## 1. The machinery — one group held back

`Av1Encoder::drain_pending` used to code the group it had just closed. It now
hands that group to a new `ready` slot and codes the group ALREADY there,
passing the just-closed group's sources as its display-FUTURE window; the
group's top ARF is its last picture in display order, so every picture of the
next group is a future neighbour of it. `code_group` (the old `drain_pending`
body) takes the group and that future slice as arguments, and `drain_all`
codes both buffered groups in coding order — what a key frame (the key
picture itself is then the last group's future) and `flush` (nothing behind
it) both need.

* **Latency**: one `mini_gop`, 8 pictures at the default pyramid, on top of
  the group the encoder already buffered.
* **Memory**: `Picture` holds u16 samples, so a 3840x2160 4:2:0 source is
  24.9 MB and the extra group is **199 MB at 4K** (56 MB at 1080p). Nothing
  else is retained: `ready` holds sources, not reconstructions.
* Output picture order, timestamps and the `show_existing_frame` headers are
  untouched — the packets are the same packets, emitted one group later.

`speed::LOOKAHEAD` (`EC_AV1_LOOKAHEAD=0|1`) switches it off: the open group is
then coded straight away, which is byte for byte the old encoder.

### The witness — `encoder::tests::the_lookahead_holds_one_group_and_still_codes_every_picture`

At 1, 7, 8, 9 and 17 pictures, at GOP 32 (one key) and GOP 8 (keys drain the
buffer mid-run), with a gradient-plus-noise fixture:

* the stream is **byte-identical** to the same run with the lookahead off, and
  the packet (order, key, level) list is identical — the delay changes nothing
  about WHAT is coded while the future window is empty;
* every run decodes to **exactly its picture count** through `decode_stream`
  AND through ffmpeg, **sample-exact between the two**, so the flush, the
  short final group (1 picture = one shown leaf, no hidden frame) and the
  display order all survive the delay;
* with ONE future neighbour the stream **MOVES** — the buffered picture is
  really consumed ([[gate-blind-to-feature]]: everything above would pass over
  a lookahead nothing reads) — and still decodes sample-exact.

RED first: in the split suite (not standalone) the future-half assertion
failed with "493 B both" — another test had left the process-global preset
high, every block coded as skip and the source filter could not change a byte
(class `process-global-knob-races-tests`). The test pins `set_speed(0)` under
`knob_write` now, the way the byte pins do; green in the suite below.

The gate is a second witness: `native_bd_arm` asserts our decoder's
display-order count and ffmpeg sample-exactness for every one of its 48-picture
streams, and all six long-GOP arms below ran RC=0.

## 2. The symmetric filter window — it pays, and it is bracketed

`speed::ARF_TF_WIN` is the PAST neighbour count, new `speed::ARF_TF_WIN_FUT`
(`EC_AV1_ARF_TF_WIN_FUT=<n>`) the FUTURE one; the caller now sizes the window
and `encode::arf_temporal_filter` takes all of it. The tpl lookahead window is
a separate argument (`encode_pyramid_inter`'s new `future` reaches the source
filter only), so this arm is the filter's window and nothing else.

Long-GOP gate, strength 2 (lane-arftf's shipped point):

| window | film A | film B |
|---|---|---|
| ±0 = 2 past (control) | +22.0 / −8.4 | +76.5 / +1.2 |
| ±1 | +21.4 / −8.8 | +74.3 / +0.3 |
| ±2 | +21.2 / −8.8 | +72.3 / −0.9 |
| **±3 (ships)** | **+21.2 / −8.8** | **+72.1 / −1.2** |
| ±4 | +21.5 / −8.7 | +72.5 / −1.0 |

**Down on all four columns against the control** (film A −0.8/−0.4, film B
−4.4/−2.4), which clears the keep rule outright, and BRACKETED on both sides —
±4 is worse than ±3 on all four, so this is not a result at its own search
edge. `arf_pred_census`'s reading is confirmed: what a wider window cost
past-only (lane-arftf refuted 4 past at +22.8/−7.8, +78.9/+2.8) it buys
symmetric.

Deviation from the charter, named: ±3 and ±4 were run on top of the chartered
±1/±2 because ±2 was the best point of a range with nothing above it.

Strength re-swept at ±2, as chartered:

| arm | film A | film B |
|---|---|---|
| ±2, strength 2 | +21.2 / −8.8 | +72.3 / −0.9 |
| ±2, strength 3 | +21.5 / −8.7 | +73.1 / −0.7 |

Worse on three columns, so `ARF_TF` stays at 2.

## 3. Guard rows — `bd_rate_screen_native`, 12 pictures, all five rows

| clip | control | shipped (lookahead + ±3) | Δ |
|---|---|---|---|
| bars 1080p | −3.5 / −19.0 | −3.4 / −19.0 | +0.1 / 0.0 |
| bars 2160p | +8.6 / −13.7 | +8.6 / −13.7 | 0.0 / 0.0 |
| film A | +17.9 / −6.4 | +17.9 / −6.3 | 0.0 / +0.1 |
| film B | +22.9 / −3.6 | +22.8 / −3.6 | −0.1 / 0.0 |
| screen capture | +14.4 / −33.2 | +14.4 / −33.2 | byte-identical |

Nothing moves by more than 0.1. Measured fact worth keeping: at 12 pictures
`group_target` absorbs the tail into ONE group of 11, so there is no next
group to buffer and **the 12-frame gate cannot see this lever at all** — a
±2 arm run at that length was byte-identical to its control on every row. The
rows above move only through the wider PAST window (2 → 3).

## 4. Invariants

* **Byte pins UNCHANGED**, green at the default and at `EC_AV1_SPEED=6`:
  `(150, 8290, 0x92cb11033d1f5813)`, `(60, 33227, 0x57ee6b1f8eacd881)`. Not a
  miss — the pin fixture is four pictures through `encode_sequence`, one
  truncated group whose ARF has two past neighbours and no next group at all,
  so neither the lookahead nor a 3-wide window can reach it.
* `encoder::tests::bitrate_target_lands_within_5_percent_over_48_frames`
  green and NOT near its bound any more: pyramid arms −0.1% (768 kbps), +0.0%
  (1536 kbps), +0.0% (2 Mbps); flat arms −2.5% / −2.8% / −1.8%.
* Split suite at the shipped default: `--skip stream::` **346 passed / 0
  failed**, `stream:: --skip 10bit` **202 passed / 0 failed**, `10bit` **42
  passed / 0 failed** (590 in all).
* `--ignored --exact
  encoder::tests::every_speed_preset_decodes_sample_exact_through_both_decoders`
  1 passed.
* `cargo check --workspace --all-targets -j4`: 0 errors, 0 `ec-av1` warnings.

## 5. What this lane did NOT do

* `deferred: the tpl window at the ARF (the charter's task 3) — the buffered
  future pictures reach the SOURCE filter only; encode_pyramid_inter still
  gets tpl_window(pos, true), the display-past leaves, for its temporal
  lambda map — unblocked by a lane with long-GOP arms to spend, and it is now
  a one-argument change (pass the same future slice as lookahead).`
* `deferred: two-pass-like allocation over the buffered group (the rate loop
  still plans per level from gop_shape, blind to the group it holds) — out of
  this charter's scope; the buffer it needs now exists.`
* `deferred: per-block weight refinement in arf_temporal_filter (libaom
  weights each 16x16 block against each neighbour separately) — untouched
  since lane-arftf.`
