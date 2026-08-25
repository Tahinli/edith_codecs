# lane-av1-predict r1 — the seven non-directional intra modes, chosen per block

## What landed

`crates/ec-av1/src/intra.rs`: the intra predictors of spec 7.11.2 that read no
further than the row above and the column to the left of a block — DC, V, H,
SMOOTH, SMOOTH_V, SMOOTH_H and PAETH — including the edge construction that
fills a missing side from the side that exists (127/129/128 where neither does).

`encode.rs` now searches those seven per 32x32 luma block: each is transformed,
quantized, reconstructed and scored as `SSE + lambda * bits`, and the winner is
committed and written as the block's luma mode. Chroma stays DC, which is the
mode the tile writer codes for it. `encode_key_frame_with_modes` exposes the mode
set so an ablation can hold the encoder at DC alone, and `Encoded::modes` reports
what each block picked.

## Gates

| gate | result |
| --- | --- |
| `cargo test --release -p ec-av1` | 73 passed, 1 ignored (the sweep probe) |
| `every_mode_decodes_to_what_the_encoder_predicted` | each of the seven forced over a whole picture, ffmpeg reconstruction sample-exact on all three planes |
| `ffmpeg_decodes_exactly_what_the_encoder_reconstructed` | sample-exact at 64x64, 96x64, 160x96 with the search live |
| `the_search_picks_the_direction_the_picture_runs` | vertical stripes pick V_PRED, horizontal pick H_PRED |
| `the_search_beats_dc_alone` | BD-rate against DC alone < -5% on the test card, < -40% on stripes |
| `a_frame_of_real_video_decodes_to_what_the_encoder_reconstructed` | sample-exact on three clips (below) |
| clippy `--all-targets` | 0 warnings |

## Mutation kills

| mutation | tests killed |
| --- | --- |
| one `Sm_Weights` entry off by one (sides 4 and 32) | 2 |
| PAETH returning above where it should return left | 1 |
| SMOOTH_V weighted by the column index instead of the row | 1 |
| V_PRED and H_PRED swapped | 3 |

The transposition mutations are the reason the striped pictures exist: every
fidelity gate survives a transposed predictor, because a symmetric picture cannot
tell the two apart. See the class note `reference-layout-not-spec`.

## The weight the search puts on rate

`LAMBDA_SCALE`, swept with `probe_lambda` over three of his clips and two
synthetic pictures — BD-rate against DC alone, negative is better:

| lambda | test card | stripes | film (Troy) | screen capture (OBS) | hand-held |
| --- | --- | --- | --- | --- | --- |
| 0 | -6.2% | -35.6% | -0.0% | -3.7% | +0.6% |
| 0.05 | **-7.8%** | **-59.5%** | **-1.1%** | **-11.5%** | **+0.0%** |
| 0.1 | -7.2% | -59.5% | -1.1% | -10.7% | +0.5% |
| 0.2 | -7.0% | -59.5% | -0.8% | -10.7% | +0.7% |
| 0.4 | -7.0% | -59.5% | -0.5% | -10.8% | +1.1% |
| 0.8 | -7.0% | -59.5% | -0.5% | -10.8% | +1.3% |

0.05 is the best point on every clip measured, so that is the default.

The hand-held clip's column is the honest part: the search can come out *behind*
DC alone there, because the cost it scores does not include what signalling a
non-DC mode costs — the mode symbol itself, and the neighbour contexts it moves.
At 0.05 that costs 0.0%; at 0.4 it costs 1.1%. Disposition: **accepted** —
the fix is to score the real symbol cost, which needs the tile writer's CDFs in
the search loop, and that is the next lane's business.

## His data

One frame of each, scaled to 640x352, at q100 / deadzone 0.5. Every one decodes
sample-exactly to what the encoder reconstructed.

| clip | bytes | luma PSNR |
| --- | --- | --- |
| Troy (film) | 18570 | 40.68 dB |
| OBS screen capture 2026-08-25 16-27-54 | 4808 | 46.06 dB |
| he_is_not_the_only_one | 1531 | 51.81 dB |

Against libaom (`-c:v libaom-av1 -cpu-used 4`, same frame of Troy):

| encoder | bytes | luma PSNR |
| --- | --- | --- |
| ours, q100 | 18570 | 40.68 |
| libaom crf 20 | 16943 | 42.91 |
| libaom crf 25 | 13733 | 41.46 |
| libaom crf 30 | 10620 | 39.75 |

Interpolated at our 40.68 dB, libaom spends about 12.3 kB against our 18.6 kB:
we cost roughly **+51% rate**, about **2.0 dB** at matched size. Before this lane
the same comparison stood at 2.4 dB.

## Not in this lane

- The eight directional modes. They read past a block's own width into
  above-right and below-left samples, whose availability the decoder tracks in
  its `BlockDecoded` bookkeeping; predicting them means reproducing that walk.
- Filter-intra and CfL (both off in the sequence header), palette, and the
  intra edge filter.
- A rate term that counts the mode symbol and the coefficient symbols as the
  writer actually codes them, rather than a magnitude estimate.
- Block sizes other than 32x32, transform-size selection, and CDF adaptation.
