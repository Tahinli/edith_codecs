# lane-refs — the reference set and the frame interpolation filter

Worktree `edith_codecs-refs`, branch `lane-refs` off main 68e96c56. Every gate
arm is the prebuilt release lib-test binary; rows are read off each log's own
header line.

## 0. Controls (12-frame native gate)

(pending — arms `rf-c1`/`rf-c2` running)

## 1. LAST2 — the census first

`encoder.rs`'s `ref_frame_idx` maps LAST2/LAST3 onto LAST's own DPB slot, so
the second past picture is not merely unoffered, it is not retained. Before
building the slot bookkeeping + a second motion search (the stage that already
owns most of the encode wall; `search_inter_block`'s `extra` references are
search-FREE for exactly that reason), `last2_census` prices the lever.

(table pending)

## 2. The frame interpolation filter

Wired: `FrameCtx::interp_filter` carries the frame's kernel to the search,
the trials, the compound predictors and the trial decode, and
`encode_inter_frame` writes the SAME kernel into the header. `EC_AV1_INTERP=
smooth|sharp` selects it; the default stays REGULAR and byte-identical.

(table pending)
