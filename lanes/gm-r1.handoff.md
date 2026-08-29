HANDOFF: Recon phase complete, zero edits, zero commits, tree clean at 2143b92. Tool-call cap (60) hit mid-recon — everything below is verified from source reads; nothing speculative.

**What the change actually is (libaom semantics nailed down):**

1. **No new warp MC path exists.** `motion_mode_allowed` (blockd.h:1472) returns SIMPLE_TRANSLATION for `is_global_mv_block` blocks (mode GLOBALMV/GLOBAL_GLOBALMV, bsize≥8x8, gm type > TRANSLATION) when `cur_frame_force_integer_mv == 0`. GLOBAL blocks' gm-derived mv goes through the **existing** `inter_predict`. warp.rs untouched.
2. **gm_get_motion_vector** (mv.h:231): IDENTITY→(0,0); TRANSLATION→ row=`wmmat[0]>>13`, col=`wmmat[1]>>13` (**spec bug: swapped axes, keep it**; GM_TRANS_ONLY_PREC_DIFF=16−3=13); ROTZOOM/AFFINE→ center point x=`mi_col*4+w/2`, y=`mi_row*4+h/2`; xc=`(mat[2]−2^16)·x+mat[3]·y+mat[0]`, yc=`mat[4]·x+(mat[5]−2^16)·y+mat[1]`; then `convert_to_trans_prec(allow_hp, ·)` = sign-rounded >>3 (hp) / >>4. `is_integer` (force_integer_mv) → full-pel rounding.
3. **Two distinct predicates — the key trap:** `is_global_mv_block` needs wmtype **> TRANSLATION** (IDENTITY and TRANSLATION both false); `is_nontrans_global_motion` (filter suppression, reconinter.h:420) excludes only wmtype **== TRANSLATION**, so **IDENTITY counts as nontrans**. Current decode.rs `resolve_interp_filter` suppress arg `is_globalmv || warped_selected` is correct under all-IDENTITY and must become `(is_globalmv && gm.model != Translation) || warped_selected`.
4. **motion_mode gate:** add `&& !(is_globalmv && gm.model > Translation)` when `!force_integer_mv` (site ~6180 `motion_mode_eligible`, plus decode_inter_block8's 8x8 leaf ~6965). GLOBALMV under TRANSLATION gm still reads OBMC/WARPED(local fit).
5. **mvstack:** neighbours in GLOBAL mode contribute the **current block's** gm_mv (not their own mv) — needs `MiInfo.is_global_mv: bool` (3 ctor sites decode.rs ~5416 compound, ~6348 single, ~6600) + `gm_mv`/`gm_active` threaded through `find_mv_stack_with_sign_bias` and `find_mv_stack_compound` into the scan/add-candidate + empty-stack pad (mvref_common.c add_ref_mv_candidate ~700-780, setup_ref_mv_list pad ~790).
6. **Compound GLOBAL_GLOBALMV:** mv=gm_mv[0], mv1=gm_mv[1] from gm of each ref (decode.rs ~5378, `is_globalmv = compound_mode == 6` already computed).
7. **Refusal:** stream.rs ~203-218 (never actually read — grep pattern failed); narrow or delete per charter step 6.

**Exact next steps (order):** (a) read stream.rs:190-230; (b) read mvref_common.c 700-800 full (artifact://5's middle was elided — add_ref_mv_candidate + pad need exact bodies, incl. whether gm candidates get clamp_mv'd); (c) decodemv.c `read_inter_compound_mode` + `av1_init_warp_params` bodies; (d) mv.h integer_mv_precision/convert_to_trans_prec + confirm WarpParams field names (frame.rs:368 `model` + params array); (e) implement in order mvstack.rs → decode.rs single-ref → compound → gates → stream.rs; (f) unit tests for gm mv (hand-computed TRANSLATION shift + ROTZOOM projection) + mvstack gm-candidate test; (g) gate ladder per charter, `--skip a_real_aomenc_stream_with_interintra_wedge`; (h) mismatch ladder EC_AV1_PREFILT_DUMP/EC_AV1_TELL. Build: `env CARGO_TARGET_DIR=$HOME/.cache/cargo-target-gm CARGO_BUILD_JOBS=4 nice -n 19 cargo test -p ec-av1 --release --lib <name> -- --nocapture`. WIP commit per green milestone; report lanes/gm-r1.report.md, verdict first line. ~46 calls remain fresh.

DELTA:
fact|decode.rs:6063 — GLOBALMV mv hardcoded (0,0), comment names stream.rs IDENTITY refusal
fact|mv.h:231 gm_get_motion_vector — IDENTITY→0; TRANSLATION row=wmmat[0]>>13 col=wmmat[1]>>13 (spec bug, keep); ROTZOOM/AFFINE center projection, convert_to_trans_prec sign-round >>3(hp)/>>4
fact|blockd.h:1472 motion_mode_allowed — is_global_mv_block→SIMPLE only when cur_frame_force_integer_mv==0; TRANSLATION-gm GLOBAL blocks still get OBMC/WARPED(local)
fact|blockd.h is_nontrans_global_motion — false only when wmtype==TRANSLATION; IDENTITY counts as nontrans; is_global_mv_block needs >TRANSLATION — two predicates, never unify
fact|reconinter.h:420 av1_is_interp_needed — skip_mode→0; nontrans-global→0; motion_mode==WARPED_CAUSAL→0
fact|mvstack port needs MiInfo.is_global_mv (ctor sites decode.rs ~5416/~6348/~6600) + gm_mv/gm_active through find_mv_stack_with_sign_bias + find_mv_stack_compound; candidates use CURRENT block's gm_mv; pad tail uses gm too (bodies NOT yet read — artifact://5 middle elided)
fact|compound GLOBAL_GLOBALMV mvs from gm_mv[0]/gm_mv[1]; read_inter_compound_mode body NOT yet read
fact|decode.rs:5615,6198 — resolve_interp_filter already takes suppress flag; narrow to (is_globalmv && model!=Translation)||warped_selected
decision|thread gm as one more decode_inter_block/decode_inter_block8 arg (house style, ~40-arg fns), no new params struct
dead-end|grep "MiInfo {" finds nothing — struct literals don't match; ctor sites are 5416/6348/6600 (sed those spans)
dead-end|shell $(grep -n …) substitution in my first two batch commands silently ran empty greps — use direct grep -n
open|gm candidates clamped in add_ref_mv_candidate? — read exact body
open|av1_init_warp_params + read_inter_compound_mode exact bodies; integer_mv_precision body; GM_*_PREC constants
open|WarpParams field names frame.rs:368 — parser fills params[idx] = (value << prec_diff) + round; wmmat[0..5] ↔ params[0..5] same order (confirm)
