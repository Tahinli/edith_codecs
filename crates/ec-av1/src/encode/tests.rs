    use super::more_tests::*;

    /// Every (site, frame type, block side) the census can be called with
    /// lands in its own ten-counter bucket, inside the array (class
    /// `enumerate-table-domain`: an off-by-one here would silently add two
    /// sites' trials together).
    #[test]
    fn the_split_census_buckets_are_distinct_and_in_bounds() {
        let mut seen = std::collections::BTreeSet::new();
        for site in 0..2 {
            for key in [true, false] {
                for side in [8usize, 16, 32, 64] {
                    let base = super::split_census_base(site, key, side);
                    assert!(
                        base + 10 <= super::SPLIT_CENSUS.len(),
                        "{site} {key} {side}"
                    );
                    assert!(seen.insert(base), "{site} {key} {side} collides");
                }
            }
        }
        assert_eq!(seen.len(), 16);
    }

    /// An [`crate::mvstack::MvStack`] with every context at zero and no
    /// neighbours, for the pricer tests below to fill the one band each of
    /// them is about.
    /// lane-b128 task 0: every `ref_mv_idx` the `NEWMV`/`NEARMV` pricer
    /// offers must be one [`crate::tile::write_drl_idx`] can really SIGNAL
    /// against the same stack -- an offer the writer clamps prices the
    /// residual against a predictor the decoder never derives, which is the
    /// silent MV drift lane-sb128b root-caused (there fixed on the WRITER
    /// side, so the stream stays legal; here the pricer's own offer set is
    /// enumerated, class `enumerate-table-domain`).
    ///
    /// **This is the coverage claim for the drl-clamp cell** (lane-av1recipehunt
    /// r2, 2026-09-29). It replaces the clip gate
    /// `a_128_superblock_clip_whose_drl_index_the_write_time_stack_cannot_carry_decodes_exact`,
    /// deleted in `3e8c65e6`+r2: that gate asserted `drl_clamp_hits() > 0` on a
    /// real 1280x768 sb128 clip, but `DRL_CLAMP_HITS` is provably unreachable
    /// while the invariant above holds, so no recipe could ever satisfy it.
    /// Deletion loses no coverage on either half:
    ///   * the clamp half -- this test IS the coverage, and it is strictly
    ///     stronger than any clip gate: it enumerates the pricer's whole offer
    ///     set against the writer's rule for every stack size 0..=4, and larger
    ///     stacks add entries the pricers never offer (`best_new_mv_syntax`
    ///     breaks at `entries.len() <= idx` for `idx > 0`, so indices 3/4 are
    ///     unreachable at ANY stack size), which is why 0..=4 bounds the
    ///     domain rather than sampling it;
    ///   * the pixel half -- the deleted gate's own decode-vs-encoder-
    ///     reconstruction compare on a 1280x768 sb128 sequence is already made
    ///     four times over, by `a_128_superblock_clip_whose_root_search_codes_
    ///     128x128_blocks_decodes_exact`,
    ///     `a_128_root_block_with_a_real_residual_decodes_exact_through_both_decoders`,
    ///     `a_128_root_residual_block_under_a_per_unit_cdef_list_decodes_exact`
    ///     and `a_rect128_half_with_a_real_residual_decodes_exact_through_both_decoders`
    ///     (same geometry, same `force_sb128(true)`, same own-reconstruction AND
    ///     ffmpeg compare).
    ///
    /// MEASURED before deleting (153 clip x size x frame-count x q combinations,
    /// `enc_probe`): `tile::drl_clamp_hits()` = 0 on every one. MUTATION PROOF
    /// the clip was never the variable: widening `best_new_mv_syntax`'s offer
    /// set to `0..5` fires the counter immediately (9/2/2/1 on
    /// mandelbrot 640x384x8, mandelbrot 320x192x8, mosaic 320x192x8,
    /// testsrc2 640x384x8) and the same clips read 0 on revert. `note_drl_clamp`
    /// and `DRL_CLAMP_HITS` are KEPT as the tripwire for the owning fix
    /// (`deferred(unblock: the DRL owner widens the pricer past the writer's
    /// `start + 2` ceiling, or retires the counter)`) -- this test will go red
    /// the moment either happens, which is the signal to re-open the cell.
    ///
    ///     cargo test -p ec-av1 --lib -- every_drl_index_the_new_mv_pricer
    #[test]
    fn every_drl_index_the_new_mv_pricer_offers_is_one_the_writer_can_signal() {
        for n in 0..=4usize {
            let mut stack = bare_stack();
            for i in 0..n {
                stack.entries.push(crate::mvstack::StackEntry {
                    mv: (8 * (i as i32 + 1), -8 * (i as i32 + 1)),
                    weight: 1,
                });
                stack.drl_ctx.push(0);
            }
            stack.nearest_mv = stack.entries.first().map_or((0, 0), |e| e.mv);
            stack.near_mv = stack.entries.get(1).map_or((0, 0), |e| e.mv);
            stack.pred_mv = stack.nearest_mv;
            // Aim the vector at each entry in turn, so every index the offer
            // set holds is the one that wins at least once.
            for target in 0..n.max(1) {
                let mv = stack.entries.get(target).map_or(stack.pred_mv, |e| e.mv);
                let (_, idx) =
                    best_new_mv_syntax(&stack, mv).expect("a zero residual is always codable");
                let idx = usize::from(idx);
                assert_eq!(
                    crate::tile::signalled_drl_idx(n, 0, idx),
                    idx,
                    "NEWMV: a {n}-entry stack cannot signal the offered drl index {idx}",
                );
            }
            // The NEARMV arm's own offer set (`search_inter_block`,
            // `search_skip_64`): indices 1 and 2, offered only when the stack
            // really holds that entry.
            for idx in 1..=2usize {
                if stack.entries.get(idx).is_some() {
                    assert_eq!(
                        crate::tile::signalled_drl_idx(n, 1, idx),
                        idx,
                        "NEARMV: a {n}-entry stack cannot signal the offered drl index {idx}",
                    );
                }
            }
        }
    }

    pub(super) fn bare_stack() -> crate::mvstack::MvStack {
        crate::mvstack::MvStack {
            entries: Default::default(),
            nearest_mv: (0, 0),
            near_mv: (0, 0),
            pred_mv: (0, 0),
            new_mv_ctx: 0,
            ref_mv_ctx: 0,
            zero_mv_ctx: 0,
            drl_ctx: Default::default(),
            above_refs: (None, None),
            left_refs: (None, None),
            above_nbr: None,
            left_nbr: None,
            above_skip: false,
            left_skip: false,
        }
    }

    /// [`super::compound_ref_bits`] pays exactly the symbols tile.rs'
    /// `write_compound_ref_frames` codes for a pair, each at that symbol's
    /// own neighbour-derived context. Red before lane-ctx, where the pricer
    /// spent four constants at CDF row 0 that assumed the pair was
    /// LAST+GOLDEN or LAST+ALTREF -- so a LAST+BWDREF pair was priced as
    /// LAST+ALTREF with its `comp_bwdref` p6 symbol missing entirely.
    #[test]
    fn the_compound_ref_pricer_pays_the_writers_own_symbol_sequence() {
        use crate::cdf::{COMP_BWDREF, COMP_MODE, COMP_REF, COMP_REF_TYPE, UNI_COMP_REF};
        use crate::encode::symbol_bits as b;
        use crate::mvstack::{
            ALTREF_FRAME, ALTREF2_FRAME, BWDREF_FRAME, GOLDEN_FRAME, LAST_FRAME, LAST2_FRAME,
            LAST3_FRAME, NeighbourRef, comp_reference_type_ctx, reference_mode_ctx,
            single_ref_p1_ctx, single_ref_p2_ctx, single_ref_p3_ctx, single_ref_p4_ctx,
            single_ref_p5_ctx, single_ref_p6_ctx, uni_comp_ref_p1_ctx,
        };
        // (pair, unidirectional?, the tree's symbols as (table, p index,
        // context selector, value)) -- transcribed from the writer.
        // selector: 1..=6 are `single_ref_p*_ctx`, 7 is `uni_comp_ref_p1_ctx`.
        type Sym = (char, usize, usize, usize);
        let cases: [((i8, i8), bool, &[Sym]); 7] = [
            (
                (LAST_FRAME, LAST2_FRAME),
                true,
                &[('u', 0, 1, 0), ('u', 1, 7, 0)],
            ),
            (
                (LAST_FRAME, LAST3_FRAME),
                true,
                &[('u', 0, 1, 0), ('u', 1, 7, 1), ('u', 2, 5, 0)],
            ),
            (
                (LAST_FRAME, GOLDEN_FRAME),
                true,
                &[('u', 0, 1, 0), ('u', 1, 7, 1), ('u', 2, 5, 1)],
            ),
            ((BWDREF_FRAME, ALTREF_FRAME), true, &[('u', 0, 1, 1)]),
            (
                (LAST_FRAME, ALTREF_FRAME),
                false,
                &[('c', 0, 3, 0), ('c', 1, 4, 0), ('b', 0, 2, 1)],
            ),
            (
                (LAST_FRAME, BWDREF_FRAME),
                false,
                &[
                    ('c', 0, 3, 0),
                    ('c', 1, 4, 0),
                    ('b', 0, 2, 0),
                    ('b', 1, 6, 0),
                ],
            ),
            (
                (LAST_FRAME, ALTREF2_FRAME),
                false,
                &[
                    ('c', 0, 3, 0),
                    ('c', 1, 4, 0),
                    ('b', 0, 2, 0),
                    ('b', 1, 6, 1),
                ],
            ),
        ];
        let nbr = |ref0, ref1: Option<i8>| {
            Some(NeighbourRef {
                is_inter: true,
                ref0,
                ref1,
                uni: ref1.is_some_and(|r1| crate::mvstack::is_uni_comp_ref(ref0, r1)),
            })
        };
        // No neighbours (every context zero), then a mixed pair that puts the
        // p* rows, `comp_reference_type_ctx` and `reference_mode_ctx` off 0.
        for (above_nbr, left_nbr, above_refs, left_refs) in [
            (None, None, (None, None), (None, None)),
            (
                nbr(LAST_FRAME, None),
                nbr(ALTREF_FRAME, Some(GOLDEN_FRAME)),
                (Some(LAST_FRAME), None),
                (Some(ALTREF_FRAME), Some(GOLDEN_FRAME)),
            ),
        ] {
            let stack = crate::mvstack::MvStack {
                above_nbr,
                left_nbr,
                above_refs,
                left_refs,
                ..bare_stack()
            };
            let (a, a1) = above_refs;
            let (l, l1) = left_refs;
            let ctx_of = |sel: usize| match sel {
                1 => single_ref_p1_ctx(a, a1, l, l1),
                2 => single_ref_p2_ctx(a, a1, l, l1),
                3 => single_ref_p3_ctx(a, a1, l, l1),
                4 => single_ref_p4_ctx(a, a1, l, l1),
                5 => single_ref_p5_ctx(a, a1, l, l1),
                6 => single_ref_p6_ctx(a, a1, l, l1),
                _ => uni_comp_ref_p1_ctx(a, a1, l, l1),
            };
            for (pair, unidir, symbols) in cases {
                let mut want = b(&COMP_MODE[reference_mode_ctx(above_nbr, left_nbr)], 1)
                    + b(
                        &COMP_REF_TYPE[comp_reference_type_ctx(above_nbr, left_nbr)],
                        usize::from(!unidir),
                    );
                for &(table, p, sel, v) in symbols {
                    let row = ctx_of(sel);
                    want += match table {
                        'u' => b(&UNI_COMP_REF[row][p], v),
                        'c' => b(&COMP_REF[row][p], v),
                        _ => b(&COMP_BWDREF[row][p], v),
                    };
                }
                let got = super::compound_ref_bits(pair, &stack);
                assert!(
                    (got - want).abs() < 1e-9,
                    "pair {pair:?} above {above_refs:?} left {left_refs:?}: \
                     priced {got} bits, writer codes {want}"
                );
            }
        }
    }

    /// The `skip` and `is_inter` symbols are priced at the SAME context
    /// tile.rs codes them with -- `above_skip + left_skip` for the first and
    /// `intra_inter_ctx` for the second -- not at row 0. Red before lane-ctx
    /// on every neighbour configuration whose context is not zero (class
    /// pricer-context-zero).
    #[test]
    fn the_skip_and_intra_inter_pricers_use_the_writers_own_contexts() {
        use crate::cdf::{INTRA_INTER, SKIP};
        use crate::encode::symbol_bits as b;
        use crate::mvstack::NeighbourRef;
        let nbr = |is_inter| NeighbourRef {
            is_inter,
            ref0: if is_inter {
                crate::mvstack::LAST_FRAME
            } else {
                0
            },
            ref1: None,
            uni: false,
        };
        for (above, left, above_skip, left_skip) in [
            (None, None, false, false),
            (Some(nbr(true)), Some(nbr(true)), true, false),
            (Some(nbr(false)), Some(nbr(false)), true, true),
            (Some(nbr(false)), None, false, true),
            (None, Some(nbr(true)), true, true),
        ] {
            let stack = crate::mvstack::MvStack {
                above_nbr: above,
                left_nbr: left,
                above_skip,
                left_skip,
                ..bare_stack()
            };
            let skip_ctx = usize::from(above_skip) + usize::from(left_skip);
            let ii_ctx = crate::tile::intra_inter_ctx(
                above.is_some(),
                left.is_some(),
                above.is_some_and(|n| n.is_inter),
                left.is_some_and(|n| n.is_inter),
            );
            for v in [false, true] {
                let (got, want) = (
                    super::skip_bits_at(&stack, v),
                    b(&SKIP[skip_ctx], usize::from(v)),
                );
                assert!(
                    (got - want).abs() < 1e-9,
                    "skip {v} at ctx {skip_ctx}: priced {got}, writer codes {want}"
                );
                let (got, want) = (
                    super::intra_inter_bits_at(&stack, v),
                    b(&INTRA_INTER[ii_ctx], usize::from(v)),
                );
                assert!(
                    (got - want).abs() < 1e-9,
                    "is_inter {v} at ctx {ii_ctx}: priced {got}, writer codes {want}"
                );
            }
        }
    }

    /// [`super::single_ref_bits`] pays exactly what tile.rs'
    /// `write_single_ref` codes: the p1/p2/p6 chain for a BACKWARD
    /// reference and the p1/p3/p4/p5 chain for a forward one, each symbol at
    /// the context that symbol's own `single_ref_p*_ctx` derives from the
    /// block's above/left neighbours. Red before lane-pricer, where every
    /// backward reference was priced as LAST (p1=0,p3=0,p4=0) at context 0.
    #[test]
    fn the_single_ref_pricer_pays_the_writers_own_symbol_sequence() {
        use crate::cdf::SINGLE_REF as SR;
        use crate::encode::symbol_bits as b;
        use crate::mvstack::{
            ALTREF_FRAME, ALTREF2_FRAME, BWDREF_FRAME, GOLDEN_FRAME, LAST_FRAME, LAST2_FRAME,
            LAST3_FRAME, MvStack, single_ref_p1_ctx, single_ref_p2_ctx, single_ref_p3_ctx,
            single_ref_p4_ctx, single_ref_p5_ctx, single_ref_p6_ctx,
        };
        let stack_with = |above_refs, left_refs, above_nbr, left_nbr| MvStack {
            above_refs,
            left_refs,
            above_nbr,
            left_nbr,
            ..bare_stack()
        };
        let nbr = |ref0, ref1| {
            Some(crate::mvstack::NeighbourRef {
                is_inter: true,
                ref0,
                ref1,
                uni: false,
            })
        };
        // (reference, the symbols `write_single_ref` emits as (p index, value))
        let cases: [(i8, &[(usize, usize)]); 7] = [
            (LAST_FRAME, &[(0, 0), (2, 0), (3, 0)]),
            (LAST2_FRAME, &[(0, 0), (2, 0), (3, 1)]),
            (LAST3_FRAME, &[(0, 0), (2, 1), (4, 0)]),
            (GOLDEN_FRAME, &[(0, 0), (2, 1), (4, 1)]),
            (BWDREF_FRAME, &[(0, 1), (1, 0), (5, 0)]),
            (ALTREF2_FRAME, &[(0, 1), (1, 0), (5, 1)]),
            (ALTREF_FRAME, &[(0, 1), (1, 1)]),
        ];
        // Both neighbours intra, then the mixed LAST-above/ALTREF-left pair
        // that puts every p* context on a different row -- the second with a
        // COMPOUND left neighbour, which is what takes `reference_mode_ctx`
        // off row 0.
        for (above_refs, left_refs, above_nbr, left_nbr) in [
            ((None, None), (None, None), None, None),
            (
                (Some(LAST_FRAME), None),
                (Some(ALTREF_FRAME), Some(LAST_FRAME)),
                nbr(LAST_FRAME, None),
                nbr(ALTREF_FRAME, Some(LAST_FRAME)),
            ),
        ] {
            let stack = stack_with(above_refs, left_refs, above_nbr, left_nbr);
            let (a, a1) = above_refs;
            let (l, l1) = left_refs;
            let ctx_of = |p: usize| match p {
                0 => single_ref_p1_ctx(a, a1, l, l1),
                1 => single_ref_p2_ctx(a, a1, l, l1),
                2 => single_ref_p3_ctx(a, a1, l, l1),
                3 => single_ref_p4_ctx(a, a1, l, l1),
                4 => single_ref_p5_ctx(a, a1, l, l1),
                _ => single_ref_p6_ctx(a, a1, l, l1),
            };
            for (ref_frame, symbols) in cases {
                // Plus the `comp_mode = 0` symbol the writer codes ahead of
                // the reference syntax on a `reference_select` frame.
                let mut want: f64 = symbols.iter().map(|&(p, v)| b(&SR[ctx_of(p)][p], v)).sum();
                if super::reference_select() {
                    want += b(
                        &crate::cdf::COMP_MODE
                            [crate::mvstack::reference_mode_ctx(above_nbr, left_nbr)],
                        0,
                    );
                }
                let got = super::single_ref_bits(ref_frame, &stack);
                assert!(
                    (got - want).abs() < 1e-9,
                    "ref {ref_frame} above {above_refs:?} left {left_refs:?}: \
                     priced {got} bits, writer codes {want}"
                );
            }
        }
    }

    /// lane-av1tpl2's propagated map, on the two properties every decision
    /// under it rests on: a frame the whole lookahead window repeats exactly
    /// (uniform texture) leans on every cell alike, so every factor is 1 and
    /// the weighting is a no-op; and where the window really does save bits
    /// by predicting -- the textured half of a frame whose other half is
    /// flat -- the factor FALLS below 1, i.e. that half is coded more
    /// distortion-averse than the half nothing gains from.
    /// The distortion metric itself (lane-sse): a block the true frame edge
    /// cuts through is scored over its INSIDE part only, so a difference that
    /// lives entirely in the padding costs nothing and a block wholly past
    /// the edge scores zero. Before this, the search ranked modes,
    /// transforms and partition trees on samples the display crop throws
    /// away -- which lane-b64b had fixed for the 64x64 root alone.
    #[test]
    fn a_block_the_frame_edge_cuts_is_scored_over_its_inside_only() {
        // A 40x40 true frame on a 64x64 padded coding surface.
        let source = vec![10u8; 64 * 64];
        let plane = fresh_plane(&source, 64, 64, 40, 40);
        let mut reconstruction = vec![10u8; 64 * 64];
        // Off by 190 at (50, 50) -- past the true edge in both axes.
        reconstruction[50 * 64 + 50] = 200;
        assert_eq!(
            plane.block_sse(0, 0, 64, &reconstruction),
            0.0,
            "padding was scored"
        );
        // Off by 10 at (39, 39) -- the last sample the decoder shows.
        reconstruction[39 * 64 + 39] = 20;
        assert_eq!(
            plane.block_sse(0, 0, 64, &reconstruction),
            100.0,
            "inside was not scored"
        );
        // A 16x16 block whose every sample is padding.
        let block = vec![200u8; 16 * 16];
        assert_eq!(
            plane.block_sse(48, 0, 16, &block),
            0.0,
            "a block past the edge scored"
        );
        // A 16x16 block the edge cuts: 8 of its columns are inside.
        assert_eq!(plane.block_sse(32, 32, 16, &block), (190.0 * 190.0) * 64.0);
    }

    /// lane-dq: libaom's objective mapping ([`super::deltaq_libaom`]) must
    /// spend WHERE THE WINDOW LEANS, stay congruent to the base on the
    /// `delta_q_res` grid, and stop at libaom's own `+-(res * 9 - 1)` bound.
    #[test]
    fn the_objective_delta_q_mapping_leans_the_right_way_and_stops_at_libaom_s_clamp() {
        let (base, res) = (162u8, 4i32);
        let rf = 2.0;
        // A superblock the window leans on more than the frame average is
        // coded FINER (lower qindex); a slack one coarser; the average one
        // exactly at the base.
        assert!(
            super::deltaq_libaom(base, 8.0, rf, 1.0, res) < base,
            "leaned-on SB must go finer"
        );
        assert!(
            super::deltaq_libaom(base, 0.2, rf, 1.0, res) > base,
            "idle SB must go coarser"
        );
        assert_eq!(
            super::deltaq_libaom(base, rf, rf, 1.0, res),
            base,
            "the average SB moves 0"
        );
        // Every offset is a whole number of steps and inside +-(res * 9 - 1).
        let max = ((res * 9 - 1) / res) * res;
        for r in [0.0f64, 0.01, 0.5, 1.0, 3.0, 50.0, 1e6] {
            for k in [0.5f64, 1.0, 1.5] {
                let q = i32::from(super::deltaq_libaom(base, r, rf, k, res));
                let d = q - i32::from(base);
                assert_eq!(
                    d % res,
                    0,
                    "r={r} k={k}: offset {d} is not a whole {res}-step"
                );
                assert!(
                    d.abs() <= max,
                    "r={r} k={k}: offset {d} past libaom's +-{max}"
                );
            }
        }
        // The clamp is REACHED from both ends, so the bound is a real bound
        // and not a formula that never gets there.
        assert_eq!(
            i32::from(super::deltaq_libaom(base, 1e9, rf, 1.0, res)) - i32::from(base),
            -max
        );
        assert_eq!(
            i32::from(super::deltaq_libaom(base, 0.0, 1e9, 1.0, res)) - i32::from(base),
            max
        );
        // A stronger `k` never moves less than a weaker one.
        let d = |k: f64| {
            (i32::from(super::deltaq_libaom(base, 8.0, rf, k, res)) - i32::from(base)).abs()
        };
        assert!(
            d(0.5) <= d(1.0) && d(1.0) <= d(1.5),
            "strength must be monotone: {} {} {}",
            d(0.5),
            d(1.0),
            d(1.5)
        );
        // The frame reference is libaom's log-mean: flat in, the same value out.
        assert!((super::tpl_frame_ratio(&[2.5; 16]) - 2.5).abs() < 1e-9);
    }

    /// lane-arfq: the per-level `delta_q` strength -- the spec parses onto
    /// [`super::DqLevel`]'s own indices, a missing or broken field keeps the
    /// shipped default, and the multiplier reaches the strength.
    #[test]
    fn the_per_level_delta_q_strength_indexes_the_levels_it_names() {
        let d = crate::speed::DQ_LEVEL_K;
        assert_eq!(super::dq_level_k_of(None), d, "no spec = the shipped table");
        assert_eq!(super::dq_level_k_of(Some("2:3:4")), [2.0, 3.0, 4.0]);
        // Short and broken fields fall back per SLOT, not per spec.
        assert_eq!(super::dq_level_k_of(Some("2")), [2.0, d[1], d[2]]);
        assert_eq!(super::dq_level_k_of(Some("2:x:-1")), [2.0, d[1], d[2]]);
        assert_eq!(super::DqLevel::TopArf as usize, 0);
        assert_eq!(super::DqLevel::MidArf as usize, 1);
        assert_eq!(super::DqLevel::Leaf as usize, 2);
    }

    #[test]
    fn tpl_factors_are_flat_on_a_uniform_repeat_and_fall_where_the_window_leans() {
        // Four 64x64 superblocks across: the shipped map forms its ratio per
        // SUPERBLOCK (`EC_AV1_TPL_SB`), so a frame one superblock wide could
        // only ever produce one ratio -- and one ratio, mean-normalised, is
        // the flat map, whatever the content underneath it.
        let (w, h) = (256usize, 64usize);
        let uniform: Vec<u8> = (0..w * h).map(|i| ((i * 37) % 251) as u8).collect();
        let frames: Vec<&[u8]> = vec![&uniform, &uniform, &uniform];
        let (f, _) = super::tpl_lambda_factors(&frames, w, h, 1.0);
        assert_eq!(f.len(), 64, "16x4 cells of 16x16");
        for (i, &v) in f.iter().enumerate() {
            assert!(
                (v - 1.0).abs() < 1e-6,
                "cell {i} factor {v}, a flat map must move no lambda"
            );
        }

        // Left half textured, right half constant: the window saves bits
        // predicting the left half and nothing at all on the right.
        let split: Vec<u8> = (0..w * h)
            .map(|i| {
                if i % w < w / 2 {
                    ((i * 37) % 251) as u8
                } else {
                    128
                }
            })
            .collect();
        let frames: Vec<&[u8]> = vec![&split, &split, &split];
        let cells_x = w / 16;
        for k in [0.25f64, 0.5, 1.0, 2.0] {
            let (f, _) = super::tpl_lambda_factors(&frames, w, h, k);
            for r in 0..h / 16 {
                let (lean, idle) = (f[r * cells_x], f[r * cells_x + cells_x - 1]);
                assert!(
                    lean < 1.0,
                    "k={k} row {r}: leaned-on cell factor {lean} must be < 1"
                );
                assert!(
                    idle > 1.0,
                    "k={k} row {r}: idle cell factor {idle} must be > 1"
                );
            }
        }
    }

    /// lane-rectx r5: libaom lays `has_tr_*`/`has_bl_*` out row-major for a
    /// 128x128 superblock -- `MAX_MIB_SIZE_LOG2 = 5` mi columns per row, so a
    /// row holds `32 >> bw_in_mi_log2` bits and the table holds
    /// `32 >> bh_in_mi_log2` such rows. Walking every block position of that
    /// superblock must therefore hit every bit of the table EXACTLY ONCE.
    /// r4's `4 - bw_log2` index halved the row stride: the walk then covered
    /// only a quarter of the 16x8 table and read the wrong byte for every
    /// block row but the first (silently wrong above-right/below-left
    /// availability -- wrong pixels, no error).
    #[test]
    fn rect_reach_tables_are_indexed_with_a_32_mi_row_stride() {
        for (bw, bh) in [
            (16usize, 8usize),
            (8, 16),
            (32, 16),
            (16, 32),
            (64, 32),
            (32, 64),
        ] {
            let (bw_log2, bh_log2) = (
                (bw / 4).trailing_zeros() as usize,
                (bh / 4).trailing_zeros() as usize,
            );
            let (rows, cols) = (32 >> bh_log2, 32 >> bw_log2);
            for (name, table) in [
                ("has_tr", rect_reach_tables(bw, bh).0),
                ("has_bl", rect_reach_tables(bw, bh).1),
            ] {
                assert_eq!(
                    table.len() * 8,
                    rows * cols,
                    "{name}_{bw}x{bh}: libaom's table is {rows} rows of {cols} bits"
                );
                let mut seen = vec![0u32; table.len() * 8];
                for blk_row in 0..rows {
                    for blk_col in 0..cols {
                        let idx = Reach::rect_table_index(bw_log2, blk_row, blk_col);
                        assert!(
                            idx < seen.len(),
                            "{name}_{bw}x{bh}: index {idx} past the table at ({blk_row},{blk_col})"
                        );
                        seen[idx] += 1;
                    }
                }
                assert!(
                    seen.iter().all(|&n| n == 1),
                    "{name}_{bw}x{bh}: the block walk is not a bijection onto the table's bits \
                     (row stride is not {cols})"
                );
            }
        }
    }
    use super::*;
    use crate::intra::{D45_PRED, D135_PRED, H_PRED, KEY_FRAME_MODES, NON_DIRECTIONAL, V_PRED};
    use crate::probe::run_with_stdin;
    use std::process::{Command, Stdio};

    /// Every rectangular shape this decoder codes, at every position a block
    /// of that shape can sit inside a 64x64 superblock: [`Reach::of_rect`]
    /// must answer exactly what libaom's `has_top_right`/`has_bottom_left`
    /// (`reconintra.c`) answer, whose bodies are ported below.
    ///
    /// lane-tx4x8 r3 ([[enumerate-table-domain]], [[tool-disabled-in-every-gate]]):
    /// `rect_reach_tables` used to route every shape but 4x8/8x4/16x32 to the
    /// 32x16 row, so live 16x8 strips read the wrong bit at 10 (above-right)
    /// and 7 (below-left) of the 21 reachable positions with no gate seeing
    /// it. The per-shape `(len, byte sum)` fingerprints below come from the
    /// oracle's own arrays, so a re-routed or truncated table fails here even
    /// where the two rows happen to agree.
    ///
    /// Not covered (no parameter for either on `of_rect`): chroma `ss_x`/
    /// `ss_y`, and sub-block transforms (`row_off`/`col_off` > 0), which the
    /// callers handle by predicting each transform unit as its own block.
    /// `PARTITION_VERT_A`/`VERT_B` need no arm either: libaom's
    /// `has_tr_vert_tables`/`has_bl_vert_tables` entry for every RECT bsize is
    /// either the plain table itself (4x8, 8x16, 16x32, 32x64) or NULL
    /// (the horizontal shapes, which those partitions never produce).
    #[test]
    fn every_rect_shape_reaches_what_libaom_says_over_the_whole_superblock() {
        // lane-b128m: this test's oracle is libaom's 64-SUPERBLOCK table, so
        // it pins the 64 reach explicitly instead of following the process's
        // own superblock knob (`for_encoder`), which made it red under
        // `EC_AV1_SB128=1` for no defect of the code under test.
        let fctx = &crate::decode::FrameCtx::new();
        // (bw, bh, has_tr len, has_tr byte sum, has_bl len, has_bl byte sum).
        const SHAPES: [(usize, usize, usize, u32, usize, u32); 14] = [
            (4, 8, 64, 9280, 64, 550),
            (8, 4, 64, 3712, 64, 9630),
            (8, 16, 16, 2352, 16, 134),
            (16, 8, 16, 960, 16, 2400),
            (16, 32, 4, 620, 4, 32),
            (32, 16, 4, 32, 4, 184),
            (32, 64, 1, 127, 1, 0),
            (64, 32, 1, 19, 1, 34),
            (4, 16, 32, 5472, 32, 14),
            (16, 4, 32, 960, 32, 6464),
            (8, 32, 8, 1400, 8, 2),
            (32, 8, 8, 32, 8, 1136),
            (16, 64, 2, 382, 2, 0),
            (64, 16, 2, 4, 2, 84),
        ];
        /// libaom `reconintra.c` `has_top_right`/`has_bottom_left`, ported for
        /// a block whose transform covers it whole (`row_off == col_off == 0`,
        /// `tx_size == bsize`) and luma (`ss_x == ss_y == 0`) on a 64-pixel
        /// superblock (`sb_mi_size == 16`).
        fn libaom_reach(
            bw: usize,
            bh: usize,
            x: usize,
            y: usize,
            width: usize,
            height: usize,
            _fctx: &crate::decode::FrameCtx,
        ) -> (bool, bool) {
            let (bw_log2, bh_log2) = ((bw / 4).ilog2() as usize, (bh / 4).ilog2() as usize);
            let (_bw_unit, bh_unit) = (bw / 4, bh / 4);
            let sb_mi_size = 16usize;
            let blk_row_in_sb = ((y / 4) & (sb_mi_size - 1)) >> bh_log2;
            let blk_col_in_sb = ((x / 4) & (sb_mi_size - 1)) >> bw_log2;
            let this_blk_index = (blk_row_in_sb << (5 - bw_log2)) + blk_col_in_sb;
            let (tr_table, bl_table) = rect_reach_tables(bw, bh);
            let bit = |t: &[u8]| (t[this_blk_index / 8] >> (this_blk_index % 8)) & 1 != 0;
            // has_top_right: top_available && right_available, then row_off == 0.
            let top_right = if y == 0 || x + bw >= width {
                false
            } else if blk_row_in_sb == 0 {
                // (`col_off + tx_wide_unit < plane_bw_unit` above this is
                // `bw_unit < bw_unit`, false for a whole-block transform.)
                true
            } else if ((blk_col_in_sb + 1) << bw_log2) >= sb_mi_size {
                false
            } else {
                bit(tr_table)
            };
            // has_bottom_left: bottom_available && left_available, col_off == 0.
            let bottom_left = if x == 0 || y + bh >= height {
                false
            } else if blk_col_in_sb == 0 {
                // (`row_off + tx_high_unit < plane_bh_unit` is likewise false.)
                (blk_row_in_sb << bh_log2) + bh_unit < sb_mi_size
            } else if ((blk_row_in_sb + 1) << bh_log2) >= sb_mi_size {
                false
            } else {
                bit(bl_table)
            };
            (top_right, bottom_left)
        }
        for (bw, bh, tr_len, tr_sum, bl_len, bl_sum) in SHAPES {
            let (tr, bl) = rect_reach_tables(bw, bh);
            let sum = |t: &[u8]| t.iter().map(|&b| u32::from(b)).sum::<u32>();
            assert_eq!(
                (tr.len(), sum(tr), bl.len(), sum(bl)),
                (tr_len, tr_sum, bl_len, bl_sum),
                "rect_reach_tables({bw}, {bh}) is not libaom's has_tr_{bw}x{bh}/has_bl_{bw}x{bh}"
            );
            // Two superblocks across and down, so the superblock-relative
            // position (and not the frame position) is what decides.
            for sb_y in [0usize, 64] {
                for sb_x in [0usize, 64] {
                    for y in (sb_y..sb_y + 64).step_by(bh) {
                        for x in (sb_x..sb_x + 64).step_by(bw) {
                            let got = Reach::of_rect(bw, bh, x, y, 256, 256, fctx);
                            let want = libaom_reach(bw, bh, x, y, 256, 256, fctx);
                            assert_eq!(
                                (got.above_right, got.below_left),
                                want,
                                "{bw}x{bh} at ({x}, {y})"
                            );
                        }
                    }
                }
            }
        }
    }

    /// Whether ffmpeg is on PATH. Absence normally SKIPs, but
    /// `EC_AV1_REQUIRE_FFMPEG=1` -- or `EC_AV1_REQUIRE_AOMENC=1`, since every
    /// aomenc gate decodes its stream through ffmpeg and is meaningless
    /// without it -- turns it into a hard failure. Without this the require
    /// flag was silently short-circuited: `!have_ffmpeg()` is evaluated first
    /// in `if !have_ffmpeg() || !have_aomenc()`, so a machine with no ffmpeg
    /// printed SKIP and reported green (class gate-skips-on-its-own-failure).
    pub(super) fn have_ffmpeg() -> bool {
        let present = Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(
            present
                || (std::env::var_os("EC_AV1_REQUIRE_FFMPEG").is_none()
                    && std::env::var_os("EC_AV1_REQUIRE_AOMENC").is_none()),
            "EC_AV1_REQUIRE_FFMPEG/EC_AV1_REQUIRE_AOMENC is set but no working ffmpeg on PATH"
        );
        present
    }

    /// Whether this ffmpeg was built with `encoder` (lane-pareto2: the
    /// SVT-AV1 Pareto rows are skipped, never failed, on a build without it).
    pub(super) fn have_encoder(encoder: &str) -> bool {
        crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args(["-hide_banner", "-encoders"])
            .stderr(Stdio::null()), None, |c| c.output())
            .is_ok_and(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .any(|l| l.split_whitespace().nth(1) == Some(encoder))
            })
    }

    /// Decodes an AV1 OBU stream with ffmpeg and hands back the three planes.
    pub(super) fn ffmpeg_decode(stream: &[u8], width: usize, height: usize) -> Picture {
        let out = run_with_stdin(
            Command::new("ffmpeg").args([
                "-v", "error", "-f", "obu", "-i", "-", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-",
            ]),
            stream,
        );
        assert!(
            out.status.success(),
            "ffmpeg refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (luma, chroma) = (width * height, width * height / 4);
        assert_eq!(
            out.stdout.len(),
            luma + 2 * chroma,
            "expected one 4:2:0 frame"
        );
        Picture {
            width,
            height,
            y: out.stdout[..luma].iter().map(|&v| u16::from(v)).collect(),
            u: out.stdout[luma..luma + chroma]
                .iter()
                .map(|&v| u16::from(v))
                .collect(),
            v: out.stdout[luma + chroma..]
                .iter()
                .map(|&v| u16::from(v))
                .collect(),
        }
    }

    /// A picture with something of everything in it: a gradient, an edge, a
    /// ripple and a block of flat colour, none of them aligned to the block
    /// grid.
    /// The in-loop filters actually FIRE and the stream still decodes to
    /// exactly what the encoder kept as its reconstruction
    /// (gate-blind-to-feature: a filter search that always answered "off"
    /// would leave every other test just as green, and the BD gate that
    /// would notice is `#[ignore]`d).
    ///
    /// Run:
    ///     cargo test -p ec-av1 --release --lib -- \
    ///         encode::tests::the_filter_search_picks_real_filters
    #[test]
    fn the_filter_search_picks_real_filters() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        let pictures: Vec<_> = (0..3).map(|i| panned_test_card(128, 128, i * 3)).collect();
        let encoded = encode_sequence_with_ctx(&pictures, 120, 0.5, fctx).unwrap();
        let fired = encoded
            .frames
            .iter()
            .filter(|f| f.loop_filter.level[0] > 0 || f.cdef.y_pri_strength[0] > 0)
            .count();
        assert!(
            fired > 0,
            "no frame of this sequence chose any in-loop filtering: {:?}",
            encoded
                .frames
                .iter()
                .map(|f| (f.loop_filter.level, f.cdef.y_pri_strength[0]))
                .collect::<Vec<_>>()
        );
        // The reconstruction the encoder predicts from is the FILTERED one,
        // and it is what a decoder of the stream produces -- byte for byte,
        // frame for frame.
        let decoded = crate::stream::decode_stream(&encoded.stream).unwrap();
        assert_eq!(decoded.len(), encoded.frames.len());
        for (i, (dec, enc)) in decoded.iter().zip(&encoded.frames).enumerate() {
            assert_eq!(dec.y, enc.reconstruction.y, "frame {i} luma");
            assert_eq!(dec.u, enc.reconstruction.u, "frame {i} U");
            assert_eq!(dec.v, enc.reconstruction.v, "frame {i} V");
        }
    }

    pub(super) fn test_card(width: usize, height: usize) -> Picture {
        let mut picture = Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let gradient = x as f64 * 200.0 / width as f64;
                let ripple = 30.0
                    * (x as f64 * std::f64::consts::PI / 23.0).sin()
                    * (y as f64 * std::f64::consts::PI / 37.0).cos();
                let edge = if x > width * 3 / 7 && y > height / 3 {
                    40.0
                } else {
                    0.0
                };
                picture.y[y * width + x] =
                    (20.0 + gradient + ripple + edge).clamp(0.0, 255.0) as u16;
            }
        }
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                let i = y * width / 2 + x;
                picture.u[i] = (100 + (x * 60 / (width / 2))) as u16;
                picture.v[i] = (200 - (y * 80 / (height / 2))) as u16;
            }
        }
        picture
    }

    pub(super) fn psnr(a: &[u16], b: &[u16]) -> f64 {
        let squared: f64 = a
            .iter()
            .zip(b)
            .map(|(&x, &y)| {
                let d = f64::from(x) - f64::from(y);
                d * d
            })
            .sum();
        if squared == 0.0 {
            return f64::INFINITY;
        }
        10.0 * (255.0 * 255.0 * a.len() as f64 / squared).log10()
    }

    /// lane-i64's witness: a smooth gradient is exactly the picture a whole
    /// 64x64 intra block predicts as well as four 32x32 ones, so the key
    /// frame's new root fires on most superblocks -- and what BOTH decoders
    /// produce is the encoder's own reconstruction, sample for sample.
    ///
    /// The fire count is the `gate-blind-to-feature` guard: without it every
    /// assert below passes on a stream that codes no whole superblock at all.
    #[test]
    fn a_smooth_gradient_key_frame_codes_whole_superblocks_and_decodes_exactly() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        // 4 x 3 superblocks, all of them wholly inside the frame.
        let (width, height) = (256usize, 192usize);
        let mut picture = Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                picture.y[y * width + x] = (16 + (x + y) * 200 / (width + height)) as u16;
            }
        }
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                picture.u[y * width / 2 + x] = (110 + x * 20 / (width / 2)) as u16;
                picture.v[y * width / 2 + x] = (130 - y * 20 / (height / 2)) as u16;
            }
        }
        let _ = take_i64_root_hits();
        let encoded = encode_key_frame_with_ctx(&picture, 120, 0.5, fctx).unwrap();
        let [roots, _offered, _edge] = take_i64_root_hits();
        // The counters are process-global, so another test encoding in
        // parallel can only ADD to them -- the per-FRAME fact is the mode
        // list, which carries one entry per coded block and so exactly 12
        // when every superblock was coded as one 64x64 intra block.
        assert_eq!(
            encoded.modes.len(),
            12,
            "not every superblock was coded as one 64x64 intra block"
        );
        assert!(
            roots >= 8,
            "a smooth-gradient key frame coded only {roots} whole 64x64 intra roots of 12"
        );
        eprintln!("whole 64x64 intra roots on a smooth gradient: {roots} of 12");

        let ours = crate::stream::decode_stream(&encoded.stream).expect("our decoder");
        assert_eq!(ours.len(), 1, "our decoder's frames");
        assert_eq!(ours[0].y, encoded.reconstruction.y, "our decoder: luma");
        assert_eq!(ours[0].u, encoded.reconstruction.u, "our decoder: U");
        assert_eq!(ours[0].v, encoded.reconstruction.v, "our decoder: V");

        if !have_ffmpeg() {
            eprintln!("SKIP the ffmpeg half: no ffmpeg");
            return;
        }
        let decoded = ffmpeg_decode(&encoded.stream, width, height);
        assert_eq!(decoded.y, encoded.reconstruction.y, "ffmpeg: luma");
        assert_eq!(decoded.u, encoded.reconstruction.u, "ffmpeg: U");
        assert_eq!(decoded.v, encoded.reconstruction.v, "ffmpeg: V");
    }

    /// The claim the whole encoder rests on: what a decoder produces is what
    /// the encoder said it would, sample for sample, on every plane.
    ///
    /// Prediction reads the reconstruction, so a single sample of drift
    /// anywhere would spread into every block below and to the right of it —
    /// which is why this is an equality and not a tolerance.
    #[test]
    fn ffmpeg_decodes_exactly_what_the_encoder_reconstructed() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        if !have_ffmpeg() {
            eprintln!("SKIP ffmpeg_decodes_exactly_what_the_encoder_reconstructed: no ffmpeg");
            return;
        }
        for &(width, height) in &[
            (64usize, 64usize),
            (96, 64),
            (160, 96),
            (32, 48),
            (248, 152),
        ] {
            let picture = test_card(width, height);
            let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
            let decoded = ffmpeg_decode(&encoded.stream, width, height);
            assert_eq!(
                decoded.y, encoded.reconstruction.y,
                "{width}x{height}: luma"
            );
            assert_eq!(decoded.u, encoded.reconstruction.u, "{width}x{height}: U");
            assert_eq!(decoded.v, encoded.reconstruction.v, "{width}x{height}: V");
        }
        // One q index from each of the four coefficient-CDF contexts
        // (0..=20, 21..=60, 61..=120, 121..=255), on a single frame size.
        let (width, height) = (64usize, 64usize);
        let picture = test_card(width, height);
        let _ = take_tx_depth_hits();
        for &q in &[15u8, 45, 100, 200] {
            let encoded = encode_key_frame_with_ctx(&picture, q, 0.5, fctx).unwrap();
            let decoded = ffmpeg_decode(&encoded.stream, width, height);
            assert_eq!(decoded.y, encoded.reconstruction.y, "q={q}: luma");
            assert_eq!(decoded.u, encoded.reconstruction.u, "q={q}: U");
            assert_eq!(decoded.v, encoded.reconstruction.v, "q={q}: V");
        }
        // lane-av1tx: the exactness above is only a claim about SPLIT
        // transforms if some block actually split one (`gate-blind-to-feature`
        // -- with `tx_depth` stuck at 0 every assert here passes on the
        // pre-lane stream). The test card's edges split at these four
        // quantizers; the counter is per-process (another test encoding in
        // parallel can only ADD to it, so this can be masked, never falsely
        // failed), so it is read once for the whole q ladder.
        let depths = take_tx_depth_hits();
        assert!(
            depths[1] + depths[2] > 0,
            "no block split its transform: depths {depths:?}"
        );
    }

    /// A synthetic screen-content picture -- a few flat colours in a grid of
    /// boxes with hard edges and text-like bars, the shape a desktop capture
    /// has -- so the palette path is exercised on content it is for.
    #[cfg(test)]
    pub(super) fn screen_card(width: usize, height: usize) -> Picture {
        const INK: [u16; 4] = [16, 90, 180, 235];
        let mut picture = Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let cell = (x / 24 + y / 24) % 2;
                let bar = usize::from(y % 24 < 6 && x % 24 >= 3 && x % 24 < 20);
                picture.y[y * width + x] = INK[cell * 2 + bar];
            }
        }
        // Four chroma colours, not two: a two-colour chroma block only ever
        // codes a `PaletteSizeUV` of 2, which leaves
        // `write_palette_colors_uv`'s cache/delta half (the n-1 shrinking
        // deltas and the neighbour cache flags, exercised only when
        // neighbouring blocks share colours) unproven by the ffmpeg oracle.
        const CHROMA_U: [u16; 4] = [96, 118, 150, 176];
        const CHROMA_V: [u16; 4] = [148, 126, 104, 92];
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                let cell = (x / 12 + 2 * (y / 12)) % 4;
                picture.u[y * (width / 2) + x] = CHROMA_U[cell];
                picture.v[y * (width / 2) + x] = CHROMA_V[cell];
            }
        }
        picture
    }

    /// The lane's own end-to-end check: on screen content the detector fires,
    /// blocks really take a palette (fire count, class
    /// `gate-blind-to-feature`), and ffmpeg -- libaom's own reader of every
    /// symbol this lane writes -- reconstructs exactly what the encoder did.
    /// The film-shaped `test_card` must NOT trip the detector, which is what
    /// keeps every non-screen stream byte-identical.
    #[test]
    fn a_screen_content_picture_codes_palette_blocks_ffmpeg_decodes_exactly() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        let (width, height) = (192usize, 96usize);
        let _ = crate::tile::take_palette_hits();
        let picture = screen_card(width, height);
        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        assert!(
            encoded.screen,
            "the detector did not fire on screen content -- no palette syntax was coded at all"
        );
        let hits = crate::tile::take_palette_hits();
        assert!(hits[0] > 0, "no block took a palette: {hits:?}");
        eprintln!("palette blocks {} sizes {:?}", hits[0], &hits[2..]);
        // The chroma half is a claim about the plane-1 syntax only if some
        // block really took a chroma palette (`gate-blind-to-feature`): every
        // exactness assert below passes on a stream that codes
        // `palette_uv_mode == 0` everywhere.
        let uv_hits = crate::tile::take_palette_uv_hits();
        assert!(
            uv_hits[0] > 0,
            "no block took a chroma palette: {uv_hits:?}"
        );
        let uv_cached = crate::tile::take_palette_uv_cache_hits();
        eprintln!(
            "chroma palette blocks {} sizes {:?}, {uv_cached} colours from the neighbour cache",
            uv_hits[0],
            &uv_hits[2..]
        );
        assert!(
            uv_cached > 0,
            "no chroma base colour came out of the neighbour cache -- \
             `write_palette_colors_uv`'s cache half is unproven by the oracle below"
        );
        let via_us = crate::stream::decode_stream(&encoded.stream).unwrap();
        assert_eq!(via_us[0].y, encoded.reconstruction.y, "our decoder: luma");
        assert_eq!(via_us[0].u, encoded.reconstruction.u, "our decoder: U");
        assert_eq!(via_us[0].v, encoded.reconstruction.v, "our decoder: V");
        if !have_ffmpeg() {
            eprintln!("SKIP the ffmpeg half: no ffmpeg");
            return;
        }
        let decoded = ffmpeg_decode(&encoded.stream, width, height);
        assert_eq!(decoded.y, encoded.reconstruction.y, "ffmpeg: luma");
        assert_eq!(decoded.u, encoded.reconstruction.u, "ffmpeg: U");
        assert_eq!(decoded.v, encoded.reconstruction.v, "ffmpeg: V");
        // A whole GOP: the inter frames' own intra blocks carry the palette
        // syntax too (the three intra arms of the inter writers), which only
        // ffmpeg reading the frames back can prove.
        let moved: Vec<Picture> = (0..4)
            .map(|f| {
                let base = screen_card(width, height);
                let mut out = Picture::grey(width, height);
                for y in 0..height {
                    for x in 0..width {
                        let sx = (x + f * 7) % width;
                        out.y[y * width + x] = base.y[y * width + sx];
                    }
                }
                for y in 0..height / 2 {
                    for x in 0..width / 2 {
                        let sx = (x + f * 3) % (width / 2);
                        out.u[y * (width / 2) + x] = base.u[y * (width / 2) + sx];
                        out.v[y * (width / 2) + x] = base.v[y * (width / 2) + sx];
                    }
                }
                out
            })
            .collect();
        let _ = crate::tile::take_palette_hits();
        let _ = crate::tile::take_palette_uv_hits();
        let gop = encode_sequence_with_ctx(&moved, 100, 0.5, fctx).unwrap();
        let gop_hits = crate::tile::take_palette_hits();
        let gop_uv_hits = crate::tile::take_palette_uv_hits();
        let inter_screen = gop.frames.iter().skip(1).filter(|f| f.screen).count();
        eprintln!(
            "GOP: {} frames with screen tools after the key frame, {} palette blocks, \
             {} chroma palette blocks",
            inter_screen, gop_hits[0], gop_uv_hits[0]
        );
        assert!(
            gop_uv_hits[0] > 0,
            "no block of the GOP took a chroma palette: {gop_uv_hits:?}"
        );
        assert!(
            inter_screen > 0,
            "no inter frame set allow_screen_content_tools"
        );
        let gop_decoded = crate::stream::decode_stream(&gop.stream).unwrap();
        assert_eq!(
            gop_decoded.len(),
            gop.frames.len(),
            "our decoder: GOP frame count"
        );
        for (i, (ours, frame)) in gop_decoded.iter().zip(gop.frames.iter()).enumerate() {
            assert_eq!(
                ours.y, frame.reconstruction.y,
                "our decoder: GOP frame {i} luma"
            );
        }
        if have_ffmpeg() {
            let ff = ffmpeg_decode_sequence(&gop.stream, width, height, moved.len());
            assert_eq!(ff.len(), gop.frames.len(), "ffmpeg: GOP frame count");
            for (i, (theirs, frame)) in ff.iter().zip(gop.frames.iter()).enumerate() {
                assert_eq!(
                    theirs.y, frame.reconstruction.y,
                    "ffmpeg: GOP frame {i} luma"
                );
                assert_eq!(theirs.u, frame.reconstruction.u, "ffmpeg: GOP frame {i} U");
                assert_eq!(theirs.v, frame.reconstruction.v, "ffmpeg: GOP frame {i} V");
            }
        }

        // The other half of the keep rule: film-shaped content leaves the
        // detector (and so the whole sequence header) alone.
        let film = encode_key_frame_with_ctx(&test_card(width, height), 100, 0.5, fctx).unwrap();
        assert!(
            !film.screen,
            "the detector fired on the film-shaped test card"
        );
    }

    /// lane-av1paletteuv: the census for `crate::tile::palette_uv_side`, the
    /// `(side / 2).max(4)` named in `lanes/av1subsizesweep.report.md` as a
    /// "4:2:2/4:4:0 palette map mis-sized" site.
    ///
    /// **What the site computes.** libaom reads the plane-1 colour-index map
    /// at `av1_get_block_dimensions(bsize, 1, xd)` (blockd.h:1512): per axis,
    /// `plane_block = block >> ss`, then `+2` when that came out under 4
    /// (the chroma sub-8x8 bump, blockd.h:1533-1541). For a SQUARE 4:2:0
    /// block both axes carry `ss = 1`, so both come to `side / 2` with the
    /// bump -- which is exactly `(side / 2).max(4)`. The value is right.
    ///
    /// **What it is not.** At 4:2:2 the correct map is `(side >> 1) x side`
    /// and at 4:4:4 it is `side x side` -- neither is square. So the value is
    /// 4:2:0-only BY CONSTRUCTION, and this gate is what keeps it that way:
    /// if an encoder that can emit another chroma shape ever appears, this
    /// reds rather than the writer silently writing a half-height map.
    ///
    /// The three arms are (1) the value matches libaom's own per-axis
    /// formula at 4:2:0 over every side the palette syntax admits, (2) no
    /// reachable writer path can be handed another chroma shape, (3) a real
    /// encode really fires chroma palettes, so arm 1 is not arithmetic on a
    /// function nothing calls (class `gate-blind-to-feature`).
    #[test]
    fn the_chroma_palette_map_side_is_the_420_plane_block_and_nothing_else_reaches_it() {
        const NAME: &str =
            "the_chroma_palette_map_side_is_the_420_plane_block_and_nothing_else_reaches_it";
        let encode_src = crate::test_source::module_source("encode");

        // ---- arm 1: the value, against libaom's formula --------------------
        // blockd.h:1530-1541, transcribed. A plane_block under 4 gains 2.
        let libaom_plane1 = |block: usize, ss: usize| {
            let plane_block = block >> ss;
            plane_block + 2 * usize::from(plane_block < 4)
        };
        let mut admitted = 0usize;
        for side in 4usize..=64 {
            // `palette_bsize_ctx`'s own domain: both sides <= 64 and
            // bw * bh >= 64 (`av1_allow_palette`). A side outside it codes no
            // palette syntax at all, so its map size is unobservable.
            if side > 64 || side * side < 64 {
                continue;
            }
            admitted += 1;
            let want_x = libaom_plane1(side, 1);
            let want_y = libaom_plane1(side, 1);
            assert_eq!(want_x, want_y, "{NAME}: 4:2:0 arms the libaom formula");
            assert_eq!(
                crate::tile::palette_uv_side(side),
                want_x,
                "{NAME}: side {side} -- palette_uv_side disagrees with libaom's plane-1 block"
            );
        }
        assert_eq!(
            admitted, 57,
            "{NAME}: the sweep admitted {admitted} sides, expected 57 (every integer 8..=64 -- \
             `palette_bsize_ctx`'s whole square domain, not just the powers of two a real block \
             takes) -- it must cover that domain, or it is not measuring it"
        );

        // Why `.max(4)` cannot fire, stated as a measurement rather than left
        // for a reader to assume: `palette_bsize_ctx` refuses `bw * bh < 64`,
        // so the smallest admitted square side is 8, whose halved plane block
        // is already 4. The floor's only reachable input would be a side
        // under 8, and libaom's own chroma sub-8x8 bump (+2) is likewise
        // unreachable there. Dropping the floor is therefore NOT a witness
        // mutation for this gate -- the mutation that bites is dropping the
        // halving (measured, see the lane report) -- and this arm says why,
        // so nobody later mistakes a green floor mutation for coverage.
        for side in [8usize, 16, 32, 64] {
            assert!(
                side / 2 >= 4 && libaom_plane1(side, 1) == side / 2,
                "{NAME}: side {side} -- the admitted domain moved, so `.max(4)` may now be \
                 reachable and the floor needs a witness mutation of its own"
            );
        }

        // The claim the gate rests on, stated as arithmetic so a future
        // reader cannot mistake "correct" for "correct at any shape": at
        // these shapes the per-axis answer is NOT this square.
        for side in [8usize, 16, 32, 64] {
            let square = crate::tile::palette_uv_side(side);
            let want_422 = (libaom_plane1(side, 1), libaom_plane1(side, 0));
            let want_444 = (libaom_plane1(side, 0), libaom_plane1(side, 0));
            assert_ne!(
                (square, square),
                want_422,
                "{NAME}: side {side} -- a square map IS the 4:2:2 map, so this gate's premise \
                 (the site is 4:2:0-only) is wrong"
            );
            assert_ne!(
                (square, square),
                want_444,
                "{NAME}: side {side} -- a square map IS the 4:4:4 map"
            );
        }

        // ---- arm 2: nothing can hand the writer another shape --------------
        // (a) Every non-test `ColorConfig` literal in the encoder names
        // subsampling (1, 1). These are the two constructors the whole
        // encoder reaches its colour config through; the test-module copies
        // are excluded by scanning only the pre-`mod tests` prefix. The cut
        // matches an inline `mod tests {` body only, so a `mod tests;` file
        // declaration does not truncate the production files after it.
        fn head(src: &str) -> &str {
            let cut = src.find("\nmod tests {").unwrap_or(src.len());
            &src[..cut]
        }
        for (label, src) in [
            ("encode.rs", head(crate::test_source::module_production_source("encode"))),
            ("encoder.rs", head(crate::test_source::module_production_source("encoder"))),
        ] {
            // Every `subsampling_*` the non-test encoder names, counted: a
            // `(0, y)` or `(1, 0)` here is a chroma shape
            // `palette_uv_side` cannot express. The one value allowed is 1
            // -- 4:2:0, the shape the square map is the right size for.
            let mut shapes: std::collections::BTreeSet<(usize, usize)> = Default::default();
            for (sx, _) in src.match_indices("subsampling_x:") {
                let rest = &src[sx + "subsampling_x:".len()..];
                let digits = |s: &str| {
                    s.trim_start()
                        .split(|c: char| !c.is_ascii_digit())
                        .next()
                        .and_then(|d| d.parse::<usize>().ok())
                        .unwrap_or(usize::MAX)
                };
                let x = digits(rest);
                let y = rest
                    .find("subsampling_y:")
                    .map(|y| digits(&rest[y + "subsampling_y:".len()..]))
                    .unwrap_or(usize::MAX);
                shapes.insert((x, y));
            }
            assert_eq!(
                shapes,
                std::collections::BTreeSet::from([(1usize, 1usize)]),
                "{NAME}: the non-test {label} names chroma shapes {shapes:?} -- any shape other \
                 than 4:2:0 is one palette_uv_side's square map cannot express (at 4:2:2 the \
                 map is (side >> 1) x side, at 4:4:4 side x side)"
            );
        }

        // (b) `key_frame_headers_colour` is the only PUBLIC entry that takes
        // a caller-chosen ColorConfig -- it is how a 4:2:2 or 4:4:4 header is
        // written at all -- so it is the one door a non-4:2:0 shape comes
        // through. It builds HEADERS only: no tile writer is reachable from
        // its body. (Its own doc says so; this measures it.)
        let headers_at = encode_src
            .find("pub fn key_frame_headers_colour(")
            .expect("key_frame_headers_colour is gone -- re-derive this arm's scan anchor");
        let body_end = encode_src[headers_at..]
            .find("\npub fn ")
            .map(|n| headers_at + n)
            .unwrap_or(encode_src.len());
        let headers_body = &encode_src[headers_at..body_end];
        for writer in [
            "write_palette_syntax",
            "write_color_index_map",
            "encode_key_frame_inner",
            "tile::",
        ] {
            assert!(
                !headers_body.contains(writer),
                "{NAME}: key_frame_headers_colour now reaches {writer} -- a caller can pair a \
                 4:2:2/4:4:4 header with a tile, and palette_uv_side writes a square map into \
                 the per-axis slot libaom reads as (side >> ss_x) x (side >> ss_y)"
            );
        }

        // (c) Every tile-producing entry point goes through
        // `encode_key_frame_inner`, whose five non-test call sites all pass
        // one of the two 4:2:0 constructors above. Counted over the
        // non-test source only, so a new road is a red rather than a line
        // somebody has to notice.
        let inner_calls = head(crate::test_source::module_production_source("encode")).matches("encode_key_frame_inner(").count()
            + head(crate::test_source::module_production_source("encoder")).matches("encode_key_frame_inner(").count()
            - 1; // the definition itself
        assert_eq!(
            inner_calls, 5,
            "{NAME}: encode_key_frame_inner has {inner_calls} non-test call sites, expected 5 -- \
             a new one must pass a colour config this gate has read"
        );

        // (d) The picture itself is 4:2:0-shaped at the door, so a 4:2:2 or
        // 4:4:4 frame cannot even be handed to an encode entry. Measured by
        // calling `check`, not by reading it.
        for (label, divisor) in [("4:2:2", 2usize), ("4:4:4", 1)] {
            let p = Picture {
                width: 64,
                height: 64,
                y: vec![0; 64 * 64],
                u: vec![0; 64 * 64 / divisor],
                v: vec![0; 64 * 64 / divisor],
            };
            assert!(
                p.check().is_err(),
                "{NAME}: Picture::check accepted a {label}-shaped picture -- if the encoder can be \
                 fed one, palette_uv_side's square map is wrong by construction"
            );
        }

        // ---- arm 3: the site really fires ---------------------------------
        // Without this, arms 1 and 2 are arithmetic and a source scan over a
        // function no encode reaches (class `gate-blind-to-feature`).
        //
        // The fire count is the DECODER's thread-local one, not the writer's
        // process-global `tile::take_palette_uv_hits`: that one is a
        // swap-to-zero, so a parallel sibling test's encode steals this test's
        // count (measured: it read `[0; 9]` under `cargo test palette`, and
        // passed alone). Round-tripping the stream instead also proves the
        // stronger half -- the reader took the map at the extent the WRITER
        // sized, which is what makes the square value right rather than merely
        // unchallenged.
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        let before = crate::decode::palette_uv_hits();
        let encoded = encode_key_frame_with_ctx(&screen_card(192, 96), 100, 0.5, fctx).unwrap();
        let decoded = crate::stream::decode_stream(&encoded.stream)
            .unwrap_or_else(|e| panic!("{NAME}: our own stream refused: {e}"));
        let fired = crate::decode::palette_uv_hits() - before;
        assert!(
            fired > 0,
            "{NAME}: no block took a chroma palette -- palette_uv_side was never exercised, so \
             this gate measures nothing"
        );
        assert_eq!(
            decoded[0].u, encoded.reconstruction.u,
            "{NAME}: the decoder did not read the chroma map at the extent the writer sized it"
        );
        assert_eq!(
            decoded[0].v, encoded.reconstruction.v,
            "{NAME}: the decoder did not read the chroma map at the extent the writer sized it"
        );
        eprintln!("{NAME}: {fired} chroma palette block(s) round-tripped");
    }

    /// The lane's keep-rule measurement: the gate's own screen capture, one
    /// KEY FRAME, coded with and without `allow_intrabc` at four quantizers.
    /// Setting the bit forces deblocking, CDEF and loop restoration off for
    /// that frame, so the table is exactly the filter loss against the bytes
    /// the copied blocks save.
    #[test]
    #[ignore = "needs the real-library manifest and ffmpeg"]
    fn probe_intrabc_key_frame() {
        let _knobs = crate::speed::knob_write();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        // lane-av1clipprobe: the manifest is a real-library file, so it
        // resolves through the crate's one fixture root (EC_FIXTURES first)
        // and its absence is reported by the one probe instead of an inline
        // SKIP that named no path. A file that IS there but unreadable is not
        // an absence, so it is a hard error rather than a second silent skip.
        let Some(manifest_path) =
            crate::library_fixture::require("real-library-manifest.tsv", "probe_intrabc_key_frame")
        else {
            return;
        };
        let manifest = std::fs::read_to_string(&manifest_path).unwrap_or_else(|e| {
            panic!(
                "probe_intrabc_key_frame: {} is present but unreadable ({e})",
                manifest_path.display()
            )
        });
        let Some(clip) = manifest
            .lines()
            .skip(1)
            .filter_map(|l| l.split('\t').next())
            .find(|p| {
                p.contains("/OBS/") && p.ends_with(".mkv") && std::path::Path::new(p).exists()
            })
        else {
            eprintln!("SKIP probe_intrabc_key_frame: no OBS recording");
            return;
        };
        let (width, height) = (1280usize, 768usize);
        let picture = clip_frame(clip, "0", width, height);
        eprintln!("| base_q_idx | intrabc | bytes | PSNR | blocks | searches/found/won |");
        for q in [5u8, 20, 35, 45] {
            for on in [false, true] {
                force_intrabc(Some(on));
                let _ = crate::tile::take_intrabc_hits();
                let _ = take_intrabc_search();
                let encoded = encode_key_frame_with_ctx(&picture, q, 0.5, fctx).unwrap();
                let hits = crate::tile::take_intrabc_hits();
                let search = take_intrabc_search();
                eprintln!(
                    "| {q} | {} | {} | {:.2} dB | {} (dv hist {:?}) | {}/{}/{} |",
                    if encoded.allow_intrabc { "on" } else { "off" },
                    encoded.stream.len(),
                    psnr_all(&encoded.reconstruction, &picture),
                    hits[0],
                    &hits[1..],
                    search[0],
                    search[1],
                    search[2],
                );
            }
        }
        force_intrabc(None);
    }

    /// lane-av1ibc's own end-to-end check: a repeated-pattern screen card
    /// sets `allow_intrabc` (so every in-loop filter is off), blocks really
    /// take a block vector (fire count + DV histogram, class
    /// `gate-blind-to-feature`), and both our own decoder and ffmpeg --
    /// libaom's reader of the `use_intrabc`/`assign_dv` syntax -- reconstruct
    /// exactly what the encoder did.
    #[test]
    fn a_repeated_pattern_key_frame_codes_intrabc_blocks_ffmpeg_decodes_exactly() {
        let _knobs = crate::speed::knob_write();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        // Wide enough for the wavefront rule (`INTRABC_DELAY_SB64` = 4
        // superblocks of 64) and tall enough for a source superblock row
        // above the blocks that copy from it.
        let (width, height) = (512usize, 256usize);
        let picture = screen_card(width, height);
        let _ = crate::tile::take_intrabc_hits();
        // The default is OFF (it measured worse on the real screen capture --
        // see [`intrabc_enabled`]); this gate proves the SYNTAX, so it turns
        // the arm on for its own encode and puts it back straight after.
        force_intrabc(Some(true));
        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        force_intrabc(None);
        assert!(
            encoded.allow_intrabc,
            "allow_intrabc was not set on a fully periodic screen card              (source repeat share {:.1}%)",
            intrabc_source_share() * 100.0
        );
        let hits = crate::tile::take_intrabc_hits();
        eprintln!(
            "intrabc blocks {} DV magnitude histogram (1,2,4,8,16,32+ px) {:?},              source repeat share {:.1}%",
            hits[0],
            &hits[1..],
            intrabc_source_share() * 100.0
        );
        let search = take_intrabc_search();
        eprintln!(
            "intrabc searches {} found-valid {} won-RD {}",
            search[0], search[1], search[2]
        );
        assert!(
            hits[0] > 0,
            "no block took a block vector -- the `use_intrabc` syntax is unproven: {hits:?}"
        );
        let via_us = crate::stream::decode_stream(&encoded.stream).unwrap();
        assert_eq!(via_us[0].y, encoded.reconstruction.y, "our decoder: luma");
        assert_eq!(via_us[0].u, encoded.reconstruction.u, "our decoder: U");
        assert_eq!(via_us[0].v, encoded.reconstruction.v, "our decoder: V");
        if !have_ffmpeg() {
            eprintln!("SKIP the ffmpeg half: no ffmpeg");
            return;
        }
        let decoded = ffmpeg_decode(&encoded.stream, width, height);
        assert_eq!(decoded.y, encoded.reconstruction.y, "ffmpeg: luma");
        assert_eq!(decoded.u, encoded.reconstruction.u, "ffmpeg: U");
        assert_eq!(decoded.v, encoded.reconstruction.v, "ffmpeg: V");
    }

    /// The minimal repro that isolated the inter-residual desync: a key frame
    /// followed by one inter frame whose single superblock carries one
    /// `NEARESTMV` block (`mv == (0, 0)`) with exactly one nonzero luma
    /// coefficient, its three sibling blocks all `skip: true`. Before the
    /// `is_inter` transform-block fix this broke ffmpeg/dav1d ("Invalid data
    /// found when processing input") even though every symbol either block
    /// codes is otherwise proven (all-skip inter blocks decode clean, and the
    /// same coefficient syntax decodes clean on every intra key-frame gate).
    #[test]
    fn a_nearestmv_block_with_one_nonzero_coefficient_decodes_clean() {
        let _knobs = crate::speed::knob_write();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_nearestmv_block_with_one_nonzero_coefficient_decodes_clean: no ffmpeg"
            );
            return;
        }
        let (width, height) = (64usize, 64usize);
        let picture = Picture::grey(width, height);
        let key = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();

        let (seq, _) = key_frame_headers(width, height, 100).unwrap();
        let (_, inter_header) = inter_frame_headers(width, height, 100, 1, 0).unwrap();

        let residual_block = BlockCoeffs {
            luma_tx_types: Vec::new(),
            angle_delta_y: 0,
            cfl_alphas: None,
            filter_intra: None,
            luma: vec![Coeff {
                row: 0,
                col: 0,
                level: 4,
            }],
            u: Vec::new(),
            v: Vec::new(),
            mode: 0,
            uv_mode: 0,
            skip: false,
            eight: None,
            dv: None,
            palette: None,
            palette_uv: None,
            tx_depth: 0,
            inter: Some(InterInfo {
                ref1: None,
                mv1: (0, 0),
                ref_frame: crate::mvstack::LAST_FRAME,
                mode: InterMode::NearestMv,
                mv: (0, 0),
                ref_mv_idx: 0,
            }),
            motion_mode: 0,
        };
        let skipped_block = BlockCoeffs {
            skip: true,
            inter: Some(InterInfo {
                ref1: None,
                mv1: (0, 0),
                ref_frame: crate::mvstack::LAST_FRAME,
                mode: InterMode::NearestMv,
                mv: (0, 0),
                ref_mv_idx: 0,
            }),
            ..BlockCoeffs::default()
        };
        let blocks = vec![
            Quadrant::Whole(residual_block),
            Quadrant::Whole(skipped_block.clone()),
            Quadrant::Whole(skipped_block.clone()),
            Quadrant::Whole(skipped_block),
        ];
        // lane-av1obmc: the header this tile is decoded under carries
        // `is_motion_mode_switchable`, so the writer must be armed with it
        // too or the decoder reads a `motion_mode` symbol nobody wrote.
        crate::tile::arm_motion_mode(inter_header.is_motion_mode_switchable);
        crate::tile::arm_warped_motion(inter_header.allow_warped_motion);
        let tile = crate::tile::sb_coeff_inter_frame_tile(
            inter_header.mi_cols,
            inter_header.mi_rows,
            100,
            &blocks,
        )
        .unwrap();

        // `key.stream` is already a temporal delimiter, the sequence header
        // and the key frame's own OBU (`encode_key_frame` built it from the
        // same `key_frame_headers(width, height, 100)` this test calls), so
        // the inter frame's OBU is appended straight onto it rather than
        // re-deriving the key frame's tile bytes.
        let mut stream = key.stream.clone();
        stream.extend_from_slice(&temporal_delimiter());
        stream.extend_from_slice(&frame_obu(&seq, &inter_header, &tile).unwrap());

        ffmpeg_decode_sequence(&stream, width, height, 2);
    }

    /// `ffprobe`'s reported `width,height` for one OBU stream -- the coded
    /// frame size an AV1 decoder allocates, not necessarily the render size
    /// (see [`a_frame_round_trips_at_its_own_size`]).
    pub(super) fn ffprobe_size(stream: &[u8]) -> (u32, u32) {
        // Distinct per call, not per process: the test binary runs callers on
        // parallel threads, and a shared name lets one test's cleanup delete
        // the stream another test's ffprobe is still reading.
        static PROBE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ec-av1-probe-{}-{}.obu",
            std::process::id(),
            PROBE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, stream).expect("writing the probe stream");
        let out = crate::tool_cache::cached_output_io(Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-f",
                "obu",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height",
                "-of",
                "csv=p=0",
            ])
            .arg(&path), None, |c| c.output())
            .expect("ffprobe failed to run");
        let _ = std::fs::remove_file(&path);
        assert!(
            out.status.success(),
            "ffprobe refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        let mut fields = text.trim().split(',');
        let width: u32 = fields.next().expect("ffprobe width").parse().unwrap();
        let height: u32 = fields.next().expect("ffprobe height").parse().unwrap();
        (width, height)
    }

    /// lane-av1cap: the final filter replay against the capture decode it
    /// replaced, at the block-padded and straddling sizes the whole-frame
    /// tests cover -- `set_verify_final_replay` makes
    /// [`pick_and_apply_filters`] run both on every frame and assert, per
    /// frame, that the spliced picture and the two filter stages the
    /// restoration search reads are bit-identical over the frame's own crop.
    #[test]
    fn the_final_filter_replay_matches_the_capture_decode_at_odd_sizes() {
        let _knobs = crate::speed::knob_write();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        crate::decode::set_verify_final_replay(true);
        for &(width, height) in &[
            (64usize, 56usize),
            (216, 96),
            (192, 120),
            (640, 352),
            (1280, 720),
        ] {
            eprintln!("--- {width}x{height}");
            let picture = test_card(width, height);
            let _ = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        }
        crate::decode::set_verify_final_replay(false);
    }

    /// A key frame at an arbitrary even size round-trips through ffmpeg over
    /// its own render rectangle: [`Encoded::reconstruction`] is exactly the
    /// picture's own size, and what ffmpeg decodes over that same top-left
    /// region equals it sample for sample.
    ///
    /// This does not check that `ffprobe` reports the picture's own size,
    /// because it does not: `ffprobe`/ffmpeg's AV1 decoder reports the coded
    /// (block-padded) frame size, the same one libaom itself would code for
    /// a non-block-aligned picture. `render_width`/`render_height` (which
    /// this crate sets correctly, spec 5.9.6) is a display hint no AV1
    /// decoder is required to crop pixels by, and ffmpeg's does not --
    /// checked empirically against ffprobe here and against a real libaom
    /// encode separately. So the coded size, not the render size, is what
    /// `ffprobe` is asserted to report below.
    #[test]
    fn a_frame_round_trips_at_its_own_size() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        if !have_ffmpeg() {
            eprintln!("SKIP a_frame_round_trips_at_its_own_size: no ffmpeg");
            return;
        }
        // 854x480 is covered separately by
        // `an_854x480_picture_round_trips_through_its_padding`, which its own
        // doc comment explains was worth keeping as its own test.
        for &(width, height) in &[
            (1920usize, 1080usize),
            (640, 352),
            (1280, 720),
            (64, 56),
            (854, 480),
            (216, 96),
            (192, 120),
        ] {
            let picture = test_card(width, height);
            let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
            assert_eq!(
                (encoded.reconstruction.width, encoded.reconstruction.height),
                (width, height),
                "{width}x{height}: reconstruction is cropped to the picture's own size"
            );
            assert_eq!(
                ffprobe_size(&encoded.stream),
                (width as u32, height as u32),
                "{width}x{height}: ffprobe reports the true (display) size, not the padded one"
            );
            let decoded = ffmpeg_decode(&encoded.stream, width, height);
            assert_eq!(
                decoded.y, encoded.reconstruction.y,
                "{width}x{height}: luma"
            );
            assert_eq!(decoded.u, encoded.reconstruction.u, "{width}x{height}: U");
            assert_eq!(decoded.v, encoded.reconstruction.v, "{width}x{height}: V");
        }
    }

    /// 854x480 (padded to 864x480, an edge-replication padded key frame) used
    /// to diverge from ffmpeg's decode at luma (row 161, col 369): a debug
    /// `aomdec` trace of `has_top_right`/`has_bottom_left` at that exact
    /// block (a 16x16 at mi (40, 88)) showed libaom indexing its pinned
    /// `has_bl_16x16` table at bit 18 (`row * 8 + col`, row=2 col=2), while
    /// `Reach::bottom_left` indexed the same table at bit 10 (`row * 4 +
    /// col`) -- different bits of an 8-byte table, one true, one false. The
    /// stride libaom indexes by is fixed at its compile-time maximum
    /// superblock (128px, `MAX_MIB_SIZE_LOG2` = 5), not the actual superblock
    /// size a stream uses, so the right stride for a 16x16 or 32x32 block is
    /// `128 / side`, not `SUPERBLOCK / side` (64-relative) -- `Reach::of`'s
    /// row/col position within the (correctly 64-relative) superblock stayed
    /// right, only the table's own row stride was wrong. Fixed in
    /// `Reach::table_stride`; not an arithmetic-coder desync (an
    /// EC_RNG-per-symbol trace against the same debug decoder found the
    /// bitstream byte-for-byte identical to libaom's across all 51188
    /// symbols of this frame before this bug was found).
    ///
    /// Since the frame header started writing the true (unpadded) display
    /// size into `frame_width`/`frame_height` rather than the padded coded
    /// size, ffmpeg crops to 854x480 on its own -- decoding at the padded
    /// 864x480 and cropping ourselves now reads past what ffmpeg actually
    /// emits (`av1_common_int.h`'s render/upscale path, mirrored by
    /// `av1_frame_size` on the decode side, crops the loop-filtered picture to
    /// the header's own size before output).
    #[test]
    fn an_854x480_picture_round_trips_through_its_padding() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        if !have_ffmpeg() {
            return;
        }
        let (width, height) = (854usize, 480usize);
        let picture = test_card(width, height);
        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        let decoded = ffmpeg_decode(&encoded.stream, width, height);
        assert_eq!(decoded.y, encoded.reconstruction.y, "854x480: luma");
        assert_eq!(decoded.u, encoded.reconstruction.u, "854x480: U");
        assert_eq!(decoded.v, encoded.reconstruction.v, "854x480: V");
    }

    /// [`Reach::of_tu`] against libaom's `has_top_right`/`has_bottom_left`
    /// transform-unit path, for every 4x4 unit of every 8x8 block of a
    /// superblock, both partition tables:
    ///
    /// 1. outside a `PARTITION_VERT_A`/`_B` it must answer exactly what the
    ///    unit-as-its-own-block `Reach::of(4, ..)` answered before r2, so
    ///    wiring the TU sites onto it changes no existing stream;
    /// 2. inside one it must not panic -- `has_tr_vert_tables[BLOCK_4X4]` is
    ///    NULL in libaom because that lookup never happens, and reading a
    ///    4x4 row out of the three-row vert tables is what crashed a real
    ///    `--enable-tx-size-search=1` stream (r2);
    /// 3. and it must be the BLOCK's vert answer where the two tables
    ///    disagree: the top-right 4x4 unit of the 8x8 block at superblock
    ///    row 1, column 0 reads its above-right samples from the 8x8 to its
    ///    upper right, which a vertical AB partition codes AFTER it
    ///    (`has_tr_vert_8x8` bit 16 = 0, ordinary `has_tr_8x8` bit 16 = 1).
    #[test]
    fn of_tu_follows_the_block_row_including_under_vert_ab() {
        let fctx = &crate::decode::FrameCtx::for_encoder();
        // The 8x8 square at superblock row 1, column 0, and the TX4 unit in
        // its top-right corner (`col_off = 4`, `row_off = 0`): that unit's
        // above-right answer is the BLOCK's, which is the whole reason a unit
        // asks the block instead of looking itself up (the
        // `has_tr_vert_*` tables have no 4x4 row at all, lane-ab16 r2).
        let plain = Reach::of(8, 0, 8, SUPERBLOCK, SUPERBLOCK, fctx);
        assert_eq!(
            Reach::of_tu(8, 8, 4, 0, 4, 4, plain).above_right,
            plain.above_right,
            "ordinary table: the top-right TX4 unit takes the block's row"
        );
        let guard = Reach::vert_ab_partition();
        let vert = Reach::of(8, 0, 8, SUPERBLOCK, SUPERBLOCK, fctx);
        assert_eq!(
            Reach::of_tu(8, 8, 4, 0, 4, 4, vert).above_right,
            vert.above_right,
            "vert AB: the unit follows the vert row the block just picked"
        );
        assert_ne!(
            plain.above_right, vert.above_right,
            "this position is the cell where the vert tables disagree; pick another if libaom's tables change"
        );
        drop(guard);
    }

    /// `Reach::top_right`/`bottom_left` against a from-scratch transcription
    /// of libaom's `has_top_right`/`has_bottom_left` (`av1/common/
    /// reconintra.c`), for every 16x16 and 32x32 block position across three
    /// superblocks square -- interior and every superblock edge (top row,
    /// left column, right column, bottom row) both sizes reach. Written to
    /// catch the class the `table_stride` bug was: an index stride silently
    /// wrong for one block size while looking plausible for the other.
    #[test]
    fn reach_matches_libaom_has_top_right_and_has_bottom_left() {
        // lane-b128m: this test's oracle is libaom's 64-SUPERBLOCK table, so
        // it pins the 64 reach explicitly instead of following the process's
        // own superblock knob (`for_encoder`), which made it red under
        // `EC_AV1_SB128=1` for no defect of the code under test.
        let fctx = &crate::decode::FrameCtx::new();
        // Transcribed from has_top_right/has_bottom_left's row_off==0,
        // col_off==0 (whole-transform) path, with MAX_MIB_SIZE_LOG2 = 5 (a
        // 128px reference grid) pinned as libaom pins it, independent of the
        // 64px superblock this crate actually codes.
        fn libaom_top_right(side: usize, x: usize, y: usize, width: usize, _height: usize) -> bool {
            if y == 0 || x + side >= width {
                return false;
            }
            let (row, col, per_side) = (
                (y % SUPERBLOCK) / side,
                (x % SUPERBLOCK) / side,
                SUPERBLOCK / side,
            );
            if row == 0 {
                return true;
            }
            if col + 1 == per_side {
                return false;
            }
            let stride = 128 / side;
            let index = row * stride + col;
            let table = if side == BLOCK {
                [95u8, 87].to_vec()
            } else {
                vec![255, 85, 119, 85, 127, 85, 119, 85]
            };
            (table[index / 8] >> (index % 8)) & 1 != 0
        }

        fn libaom_bottom_left(
            side: usize,
            x: usize,
            y: usize,
            height: usize,
            _fctx: &crate::decode::FrameCtx,
        ) -> bool {
            if x == 0 || y + side >= height {
                return false;
            }
            let (row, col, per_side) = (
                (y % SUPERBLOCK) / side,
                (x % SUPERBLOCK) / side,
                SUPERBLOCK / side,
            );
            if col == 0 {
                return row * side + side < SUPERBLOCK;
            }
            if row + 1 == per_side {
                return false;
            }
            let stride = 128 / side;
            let index = row * stride + col;
            let table = if side == BLOCK {
                [4u8, 4].to_vec()
            } else {
                vec![84, 16, 84, 0, 84, 16, 84, 0]
            };
            (table[index / 8] >> (index % 8)) & 1 != 0
        }

        let (width, height) = (SUPERBLOCK * 3, SUPERBLOCK * 3);
        for side in [16usize, BLOCK] {
            for y in (0..height).step_by(side) {
                for x in (0..width).step_by(side) {
                    let reach = Reach::of(side, x, y, width, height, fctx);
                    assert_eq!(
                        reach.above_right,
                        libaom_top_right(side, x, y, width, height),
                        "side={side} x={x} y={y}: above_right"
                    );
                    assert_eq!(
                        reach.below_left,
                        libaom_bottom_left(side, x, y, height, fctx),
                        "side={side} x={x} y={y}: below_left"
                    );
                }
            }
        }
    }

    /// lane-part32 r6: inside a `PARTITION_VERT_A`/`_B` the 8x8 squares are
    /// visited TL, BL, TR, BR, so the TR square (even 8x8 row, odd 8x8
    /// column inside the superblock) may read the below-left samples the
    /// raster table forbids: `has_bl_8x8` is 0 at exactly those 16 slots
    /// where `has_bl_vert_8x8` is 1. Pinned here because no aomenc recipe
    /// swept in r6 produced a decodable 16x16-level VERT_B stream (every
    /// stream that partitions that small first hits the still-refused
    /// HORZ_A/HORZ_B/VERT_A-below-16 or coded-rect-strip-below-16 arms).
    /// [`Reach::of_tu`] against a from-scratch transcription of libaom's
    /// `has_top_right`/`has_bottom_left` TU branches (`reconintra.c`, luma,
    /// `ss_x = ss_y = 0`), which work in MI units where `of_tu` works in
    /// pixels -- lane-palette2 r12: the square multi-TU path used the
    /// standalone-block tables instead of this rule and granted a 16x16
    /// D203 block's top-right 8x8 unit bottom-left pixels libaom refuses.
    #[test]
    fn of_tu_matches_libaom_has_top_right_and_has_bottom_left_per_unit() {
        // libaom, in MI units: bw_unit/bh_unit = block size / 4,
        // *_count_unit = tx size / 4.
        fn libaom_tr(
            bw: usize,
            bh: usize,
            col_off: usize,
            row_off: usize,
            tx: usize,
            blk: bool,
        ) -> bool {
            let (plane_bw_unit, count) = (bw / 4, tx / 4);
            let (col_off_u, row_off_u) = (col_off / 4, row_off / 4);
            let _ = bh;
            if row_off_u > 0 {
                col_off_u + count < plane_bw_unit
            } else if col_off_u + count < plane_bw_unit {
                true
            } else {
                blk
            }
        }
        fn libaom_bl(
            bw: usize,
            bh: usize,
            col_off: usize,
            row_off: usize,
            tx: usize,
            blk: bool,
        ) -> bool {
            let (plane_bh_unit, count) = (bh / 4, tx / 4);
            let (col_off_u, row_off_u) = (col_off / 4, row_off / 4);
            let _ = bw;
            if col_off_u > 0 {
                false
            } else if row_off_u + count < plane_bh_unit {
                true
            } else {
                blk
            }
        }
        for &(bw, bh) in &[
            (8, 8),
            (16, 16),
            (32, 32),
            (64, 64),
            (16, 8),
            (8, 16),
            (32, 16),
            (16, 32),
            (64, 32),
            (32, 64),
        ] {
            for &tx in &[4usize, 8, 16, 32] {
                if tx > bw.min(bh) {
                    continue;
                }
                for row_off in (0..bh).step_by(tx) {
                    for col_off in (0..bw).step_by(tx) {
                        for &blk in &[
                            Reach {
                                above_right: false,
                                below_left: false,
                            },
                            Reach {
                                above_right: true,
                                below_left: true,
                            },
                        ] {
                            let got = Reach::of_tu(bw, bh, col_off, row_off, tx, tx, blk);
                            assert_eq!(
                                got.above_right,
                                libaom_tr(bw, bh, col_off, row_off, tx, blk.above_right),
                                "above_right bw={bw} bh={bh} tx={tx} row_off={row_off} col_off={col_off} blk={blk:?}"
                            );
                            assert_eq!(
                                got.below_left,
                                libaom_bl(bw, bh, col_off, row_off, tx, blk.below_left),
                                "below_left bw={bw} bh={bh} tx={tx} row_off={row_off} col_off={col_off} blk={blk:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn vert_ab_partition_flips_below_left_for_the_top_right_8x8() {
        let fctx = &crate::decode::FrameCtx::for_encoder();
        let (width, height) = (SUPERBLOCK * 3, SUPERBLOCK * 3);
        // Interior superblock, so neither the frame edge nor the col == 0 /
        // last-row early returns answer instead of the table.
        let (sb_x, sb_y) = (SUPERBLOCK, SUPERBLOCK);
        let mut flipped = 0;
        for row in 0..8 {
            for col in 1..8 {
                let (x, y) = (sb_x + col * 8, sb_y + row * 8);
                let raster = Reach::of(8, x, y, width, height, fctx).below_left;
                let vert = {
                    let _guard = Reach::vert_ab_partition();
                    Reach::of(8, x, y, width, height, fctx).below_left
                };
                // The TR square of a VERT_A/_B sits at an even row, odd
                // column; every other slot must be untouched by the guard.
                if row % 2 == 0 && col % 2 == 1 {
                    assert!(!raster, "raster below_left at row={row} col={col}");
                    assert!(vert, "vert below_left at row={row} col={col}");
                    flipped += 1;
                } else {
                    assert_eq!(raster, vert, "guard moved row={row} col={col}");
                }
            }
        }
        assert_eq!(flipped, 4 * 4, "expected the 16 8x8 TR slots to flip");
        // The guard is scoped: it must not leak past its own block.
        assert!(!Reach::of(8, sb_x + 8, sb_y, width, height, fctx).below_left);
    }

    /// An odd width or height is refused by name, not by however the padder
    /// or the block coder would happen to fail on it.
    #[test]
    fn odd_dimensions_are_refused() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        for &(width, height) in &[(1921usize, 1080usize), (1920, 1081), (63, 63)] {
            let picture = Picture::grey(width, height);
            let err = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx)
                .expect_err(&format!("{width}x{height} is odd and must be refused"));
            assert!(
                err.to_string().contains("even"),
                "{width}x{height}: error was {err}"
            );
        }
    }

    /// A sequence at a size that is not a multiple of the block grid decodes
    /// to the right frame count and the right size for every frame,
    /// including the inter frames whose reference is the previous frame's
    /// own (padded) reconstruction.
    #[test]
    fn sequence_round_trips_at_a_non_multiple_size() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        if !have_ffmpeg() {
            eprintln!("SKIP sequence_round_trips_at_a_non_multiple_size: no ffmpeg");
            return;
        }
        let (width, height) = (160usize, 96usize);
        let pictures: Vec<Picture> = (0..3)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
        assert_eq!(encoded.frames.len(), 3);
        for (i, frame) in encoded.frames.iter().enumerate() {
            assert_eq!(
                (frame.reconstruction.width, frame.reconstruction.height),
                (width, height),
                "frame {i}: reconstruction size"
            );
        }
        // See `a_frame_round_trips_at_its_own_size`: `ffprobe` reports the
        // true (display) size a sequence's frames share, not the padded one.
        assert_eq!(
            ffprobe_size(&encoded.stream),
            (width as u32, height as u32),
            "sequence: ffprobe reports the true (display) size"
        );
        let decoded = ffmpeg_decode_sequence(&encoded.stream, width, height, 3);
        assert_eq!(decoded.len(), 3, "decoded frame count");
        for (i, (frame, decoded)) in encoded.frames.iter().zip(&decoded).enumerate() {
            assert_eq!(decoded.y, frame.reconstruction.y, "frame {i}: luma");
            assert_eq!(decoded.u, frame.reconstruction.u, "frame {i}: U");
            assert_eq!(decoded.v, frame.reconstruction.v, "frame {i}: V");
        }
    }

    /// 1280x720 -- the export size a real edith timeline hits -- is exactly
    /// the "half straddle" class: 720 mod 32 == 16, so the true frame edge
    /// falls at exactly half of the last row of 32x32 blocks (`has_half`'s
    /// `pos + side_mi / 2 < bound` boundary), which is the case
    /// `sb_coeff_inter_frame_tile`'s `Quadrant::Split` now codes rather than
    /// refusing. A key frame alone already covered this size; this proves the
    /// inter tile writer's parity with it across a real multi-frame export.
    #[test]
    fn a_sequence_round_trips_at_the_exactly_half_straddle_size() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        if !have_ffmpeg() {
            eprintln!("SKIP a_sequence_round_trips_at_the_exactly_half_straddle_size: no ffmpeg");
            return;
        }
        let (width, height) = (1280usize, 720usize);
        let pictures: Vec<Picture> = (0..3)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
        assert_eq!(encoded.frames.len(), 3);
        for (i, frame) in encoded.frames.iter().enumerate() {
            assert_eq!(
                (frame.reconstruction.width, frame.reconstruction.height),
                (width, height),
                "frame {i}: reconstruction size"
            );
        }
        assert_eq!(
            ffprobe_size(&encoded.stream),
            (width as u32, height as u32),
            "ffprobe reports the true (display) 1280x720 size"
        );
        let decoded = ffmpeg_decode_sequence(&encoded.stream, width, height, 3);
        assert_eq!(decoded.len(), 3, "decoded frame count");
        for (i, (frame, decoded)) in encoded.frames.iter().zip(&decoded).enumerate() {
            assert_eq!(decoded.y, frame.reconstruction.y, "frame {i}: luma");
            assert_eq!(decoded.u, frame.reconstruction.u, "frame {i}: U");
            assert_eq!(decoded.v, frame.reconstruction.v, "frame {i}: V");
        }
    }

    /// 640x360 is the class the charter names: 360 mod 32 == 8, so a 16x16
    /// leaf's own half (`has_half` at the SUB level) itself straddles the
    /// true edge on one axis only. lane-av1-rect r7 wired the key frame
    /// writer's 8x8-leaf path (`crate::tile::write_leaf8`) live for exactly
    /// this case; lane-av1inter8 extends the same leaf-8 path to inter
    /// frames (`code_square_inter` at `side == 8`, `sb_coeff_inter_frame_tile`'s
    /// straddling-16x16 branch), so an inter frame no longer refuses this
    /// size either. This only proves the encoder does not error out -- see
    /// `a_640x360_half_straddle_sequence_round_trips_through_ffmpeg` for the
    /// decoder-clean proof (a 3-frame sequence, so the inter path is
    /// actually exercised).
    #[test]
    fn key_frame_and_inter_frame_both_encode_at_half_straddle_size() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        let (width, height) = (640usize, 360usize);
        let picture = Picture::grey(width, height);
        encode_key_frame_with_ctx(&picture, 100, 0.5, fctx)
            .expect("key frame now codes the mod-32==8 straddle");

        // `encode_sequence`'s second frame is the inter path: same size, now
        // wired the same way.
        let pictures = vec![picture.clone(), picture];
        encode_sequence_with_ctx(&pictures, 100, 0.5, fctx)
            .expect("inter frame now codes the mod-32==8 straddle too");
    }

    /// The r15 fix at production size: three 640x360 key frames (inter is
    /// still refused at this size, per the test above, so this stands in for
    /// the charter's "3-frame sequence" using three independent key frames of
    /// shifted content), each proven decoder-clean through ffmpeg rather than
    /// only encoder-clean.
    #[test]
    fn a_640x360_half_straddle_round_trips_through_ffmpeg_across_three_frames() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_640x360_half_straddle_round_trips_through_ffmpeg_across_three_frames: \
                 no ffmpeg"
            );
            return;
        }
        let (width, height) = (640usize, 360usize);
        for shift in [0i64, 7, 19] {
            let picture = panned_test_card(width, height, shift);
            let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
            let decoded = ffmpeg_decode(&encoded.stream, width, height);
            assert_eq!(decoded.y, encoded.reconstruction.y, "shift {shift}: luma");
            assert_eq!(decoded.u, encoded.reconstruction.u, "shift {shift}: U");
            assert_eq!(decoded.v, encoded.reconstruction.v, "shift {shift}: V");
        }
    }

    /// The 640x360 straddle class, but through the inter path an actual
    /// 3-frame sequence exercises (lane-av1inter8's own ladder rung (b)):
    /// proves the leaf-8 inter stream this round wires up is not just
    /// encoder-clean (the test above) but decoder-clean against ffmpeg too.
    #[test]
    fn a_640x360_half_straddle_sequence_round_trips_through_ffmpeg() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_640x360_half_straddle_sequence_round_trips_through_ffmpeg: no ffmpeg"
            );
            return;
        }
        let (width, height) = (640usize, 360usize);
        let pictures: Vec<Picture> = (0..4)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
        assert_eq!(encoded.frames.len(), 4);
        if let Ok(path) = std::env::var("EC_AV1_DUMP") {
            std::fs::write(&path, &encoded.stream).expect("dump the raw stream");
        }
        let decoded = ffmpeg_decode_sequence(&encoded.stream, width, height, 4);
        assert_eq!(decoded.len(), 4, "decoded frame count");
        for (i, (frame, decoded)) in encoded.frames.iter().zip(&decoded).enumerate() {
            assert_eq!(decoded.y, frame.reconstruction.y, "frame {i}: luma");
            assert_eq!(decoded.u, frame.reconstruction.u, "frame {i}: U");
            assert_eq!(decoded.v, frame.reconstruction.v, "frame {i}: V");
        }
    }

    /// One frame of a real clip, scaled to a whole number of 32x32 blocks.
    pub(super) fn clip_frame(clip: &str, skip: &str, width: usize, height: usize) -> Picture {
        let out = crate::tool_cache::cached_output_io(Command::new("ffmpeg")
            .args(["-v", "error", "-ss", skip, "-i", clip, "-frames:v", "1"])
            .args(["-vf", &format!("scale={width}:{height}")])
            .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"]), None, |c| c.output())
            .expect("ffmpeg failed to run");
        assert!(
            out.status.success(),
            "ffmpeg: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (luma, chroma) = (width * height, width * height / 4);
        assert_eq!(
            out.stdout.len(),
            luma + 2 * chroma,
            "expected one 4:2:0 frame"
        );
        Picture {
            width,
            height,
            y: out.stdout[..luma].iter().map(|&v| u16::from(v)).collect(),
            u: out.stdout[luma..luma + chroma]
                .iter()
                .map(|&v| u16::from(v))
                .collect(),
            v: out.stdout[luma + chroma..]
                .iter()
                .map(|&v| u16::from(v))
                .collect(),
        }
    }

    /// Prints what the mode search saves over DC prediction alone, for the
    /// synthetic pictures and for whatever clips `EC_AV1_CLIPS` names, so the
    /// weight the search puts on rate can be swept.
    #[test]
    #[ignore = "a sweep, not a gate"]
    fn probe_lambda() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        let mut pictures = vec![
            ("test card".to_string(), test_card(160, 96)),
            ("stripes".to_string(), stripes(160, 96, true)),
        ];
        if let Ok(clips) = std::env::var("EC_AV1_CLIPS") {
            for entry in clips.split(':').filter(|e| !e.is_empty()) {
                let (path, skip) = entry.split_once('@').unwrap_or((entry, "0"));
                let name = path.rsplit('/').next().unwrap_or(path).to_string();
                pictures.push((name, clip_frame(path, skip, 640, 352)));
            }
        }
        for (name, picture) in pictures {
            let dc = ladder(&picture, &[DC_PRED], fctx);
            let searched = ladder(&picture, &NON_DIRECTIONAL, fctx);
            println!(
                "{name}: {:+.1}% rate against DC alone, {:.0}B/{:.2}dB against {:.0}B/{:.2}dB at the middle point",
                bd_rate(&dc, &searched) * 100.0,
                10f64.powf(searched[1].1),
                searched[1].0,
                10f64.powf(dc[1].1),
                dc[1].0,
            );
        }
    }

    /// Prints the whole mode-search ladder, so two builds of the search can be
    /// compared against each other rather than each against its own baseline.
    #[test]
    #[ignore = "a sweep, not a gate"]
    fn probe_ladder() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        for (name, picture) in sweep_pictures() {
            let all = ladder(&picture, &KEY_FRAME_MODES, fctx);
            let points = all
                .iter()
                .map(|&(db, log_bytes)| format!("{:.0}@{db:.3}", 10f64.powf(log_bytes)))
                .collect::<Vec<_>>()
                .join(" ");
            println!("ladder {name}: {points}");
        }
    }

    /// Splitting a 32x32 block into four 16x16 ones, measured both ways over
    /// the sweep pictures and whatever clips `EC_AV1_CLIPS` names. This is what
    /// sets [`SPLIT_BLOCKS`]; the table it prints is in the lane report.
    #[test]
    // ROBUST-RED on this tree, and the red is in the MEASURE, not the codec:
    // the `stripes` picture codes LOSSLESSLY at q110/90/70, so both ladders
    // come back as three identical `(inf, 1.8260748027008264)` points and
    // `bd_rate` has an empty trapezoid to integrate, which panics with
    // "the two ladders have to overlap in PSNR". Row 1 (`test card`) still
    // prints its number before the panic. Point `EC_AV1_CLIPS` at a real lossy
    // clip, or read only row 1.
    #[ignore = "a sweep, not a gate; PANICS on its own lossless synthetic rows (stripes is lossless at q110/90/70, so bd_rate has no PSNR overlap) -- point EC_AV1_CLIPS at a lossy clip"]
    pub(super) fn probe_split() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        for (name, picture) in sweep_pictures() {
            let ladder = |split: bool| {
                let mut points: Vec<(f64, f64)> = [110u8, 90, 70]
                    .iter()
                    .map(|&q| {
                        let encoded = encode_key_frame_inner(
                            &picture,
                            q,
                            0.5,
                            &KEY_FRAME_MODES,
                            split,
                            (picture.width, picture.height),
                            unspecified_color_config(),
                            fctx,
                        )
                        .unwrap();
                        (
                            psnr(&encoded.reconstruction.y, picture.y.as_slice()),
                            (encoded.stream.len() as f64).log10(),
                        )
                    })
                    .collect();
                points.sort_by(|a, b| a.0.total_cmp(&b.0));
                points
            };
            let (whole, split) = (ladder(false), ladder(true));
            let blocks = encode_key_frame_inner(
                &picture,
                90,
                0.5,
                &KEY_FRAME_MODES,
                true,
                (picture.width, picture.height),
                unspecified_color_config(),
                fctx,
            )
            .unwrap()
            .modes
            .len();
            let quadrants = (picture.width / BLOCK) * (picture.height / BLOCK);
            println!(
                "split {name}: {:+.2}% rate, {blocks} blocks for {quadrants} quadrants at q90",
                bd_rate(&whole, &split) * 100.0
            );
        }
    }

    /// A striped picture, running one way or the other. The stripes are what
    /// separates a vertical predictor from a horizontal one: a mode search
    /// reading a transposed edge would pick the wrong one of the pair, which no
    /// symmetric picture would show.
    pub(super) fn stripes(width: usize, height: usize, vertical: bool) -> Picture {
        let mut picture = Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let along = if vertical { x } else { y };
                picture.y[y * width + x] = if (along / 4) % 2 == 0 { 40 } else { 210 };
            }
        }
        picture
    }

    /// Where two planes first disagree, and by how much: a mismatch reported as
    /// a position says which block and which sample of it went wrong, which a
    /// pair of thousand-sample arrays does not. A length difference counts as a
    /// disagreement: `zip` stops at the shorter plane, so a short one would
    /// otherwise compare exact over the prefix the two share.
    pub(super) fn first_difference(ours: &[u16], theirs: &[u16], width: usize) -> Option<String> {
        if let Some(i) = ours.iter().zip(theirs).position(|(a, b)| a != b) {
            let differ = ours.iter().zip(theirs).filter(|(a, b)| a != b).count();
            return Some(format!(
                "{differ} samples differ, first at ({}, {}): ours {} theirs {}",
                i % width,
                i / width,
                ours[i],
                theirs[i]
            ));
        }
        if ours.len() != theirs.len() {
            return Some(format!(
                "no sample in the shared prefix differs, but ours is {} samples and theirs is {}",
                ours.len(),
                theirs.len()
            ));
        }
        None
    }

    /// Every mode the encoder offers has to predict what the decoder predicts,
    /// not just the ones a particular picture happens to choose: each is forced
    /// over a whole picture and the reconstruction gated against ffmpeg.
    #[test]
    fn every_mode_decodes_to_what_the_encoder_predicted() {
        let _knobs = crate::speed::knob_write();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        if !have_ffmpeg() {
            eprintln!("SKIP every_mode_decodes_to_what_the_encoder_predicted: no ffmpeg");
            return;
        }
        // A mode ablation measures the MODE search: a filter-intra block is
        // coded `DC_PRED` whatever mode the arm forces, so it would fail the
        // "the encoder coded something else" check for a reason that is not
        // about the mode search at all.
        force_filter_intra(Some(false));
        // 128 wide is a whole number of superblocks and 160 is not: the last
        // superblock of a 160-wide row is half a one, whose blocks have no
        // above-right samples inside the frame at all.
        for (width, height) in [(128, 96), (160, 96)] {
            let picture = test_card(width, height);
            for mode in KEY_FRAME_MODES {
                let encoded =
                    encode_key_frame_with_modes_with_ctx(&picture, 100, 0.5, &[mode], fctx)
                        .unwrap();
                let decoded = ffmpeg_decode(&encoded.stream, width, height);
                for (plane, ours, theirs, stride) in [
                    ("luma", &encoded.reconstruction.y, &decoded.y, width),
                    ("U", &encoded.reconstruction.u, &decoded.u, width / 2),
                    ("V", &encoded.reconstruction.v, &decoded.v, width / 2),
                ] {
                    assert!(
                        first_difference(ours, theirs, stride).is_none(),
                        "{width}x{height} mode {mode}, {plane}: {}",
                        first_difference(ours, theirs, stride).unwrap()
                    );
                }
                assert!(
                    encoded.modes.iter().all(|&m| m == mode),
                    "mode {mode}: the encoder coded something else"
                );
            }
        }
    }

    /// The search has to follow the picture: vertical stripes are cheapest
    /// predicted from the row above, horizontal ones from the column to the
    /// left. Reading the edges the other way round would swap the two answers
    /// while leaving every fidelity gate intact.
    #[test]
    fn the_search_picks_the_direction_the_picture_runs() {
        let _knobs = crate::speed::knob_write();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        // Same confound as [`every_mode_decodes_to_what_the_encoder_predicted`]:
        // a filter-intra block is coded `DC_PRED` whatever mode the arm
        // offers, and these stripes are SCREEN content, where the filter arm
        // now searches its own transform depth (lane-fitu) and wins outright
        // -- which says nothing about the mode search this gate measures.
        force_filter_intra(Some(false));
        for (vertical, want) in [(true, V_PRED), (false, H_PRED)] {
            let picture = stripes(128, 96, vertical);
            // Only the pair, because on stripes a third mode predicts exactly
            // what the right one of the pair does -- PAETH reads the corner and
            // the left column, which a striped picture makes equal -- and the
            // tie then goes to whichever is cheaper to name, which is not what
            // this gate is about.
            let encoded =
                encode_key_frame_with_modes_with_ctx(&picture, 100, 0.5, &[V_PRED, H_PRED], fctx)
                    .unwrap();
            // The first block of the picture has neither neighbour, so it
            // cannot tell the modes apart; every other one can.
            let picked = encoded.modes[1..].iter().filter(|&&m| m == want).count();
            assert!(
                picked * 2 > encoded.modes.len() - 1,
                "vertical={vertical}: only {picked} of {} blocks picked mode {want}, modes {:?}",
                encoded.modes.len() - 1,
                encoded.modes
            );
        }
    }

    /// What the search is for: the same picture, at the same quantizer, coded
    /// smaller and more faithfully than DC alone can manage.
    /// Rate saved at matched fidelity, over a three-point ladder: the trapezoid
    /// between the two rate-distortion curves in log-rate against PSNR, as a
    /// fraction of the reference's rate. Negative means the second curve costs
    /// less for the same picture.
    pub(super) fn bd_rate(reference: &[(f64, f64)], other: &[(f64, f64)]) -> f64 {
        let low = reference[0].0.max(other[0].0);
        let high = reference[reference.len() - 1]
            .0
            .min(other[other.len() - 1].0);
        // lane-av1tx: `TxMode::Select` moved the directional ladder clear of
        // the non-directional one on the synthetic diagonals -- every point
        // both higher in PSNR *and* lower in rate -- so there is no PSNR band
        // to integrate over any more. A curve that dominates that way has
        // saved everything the measure can express (the
        // `instrument-at-bound` class: the answer is at the instrument's own
        // edge, not undefined), so name it rather than panicking; anything
        // less clean than strict dominance still is.
        if high <= low {
            let ref_lo_rate = reference.iter().map(|p| p.1).fold(f64::MAX, f64::min);
            let other_hi_rate = other.iter().map(|p| p.1).fold(f64::MIN, f64::max);
            let dominates =
                other[0].0 > reference[reference.len() - 1].0 && other_hi_rate <= ref_lo_rate;
            assert!(
                dominates,
                "the two ladders have to overlap in PSNR: {reference:?} vs {other:?}"
            );
            return -1.0;
        }
        let log_rate_at = |curve: &[(f64, f64)], psnr: f64| {
            let i = curve
                .windows(2)
                .position(|w| psnr >= w[0].0 && psnr <= w[1].0)
                .unwrap_or(0);
            let (x0, y0) = curve[i];
            let (x1, y1) = curve[i + 1];
            y0 + (y1 - y0) * (psnr - x0) / (x1 - x0)
        };
        let steps = 64;
        let mut area = 0.0;
        for step in 0..steps {
            let psnr = low + (high - low) * (f64::from(step) + 0.5) / f64::from(steps);
            area += log_rate_at(other, psnr) - log_rate_at(reference, psnr);
        }
        10f64.powf(area / f64::from(steps)) - 1.0
    }

    /// A ladder of (luma PSNR, log10 bytes) for one mode set, ordered by
    /// fidelity.
    pub(super) fn ladder(picture: &Picture, modes: &[u8], fctx: &crate::decode::FrameCtx) -> Vec<(f64, f64)> {
        // A mode ablation measures the MODE search: the palette would code
        // these synthetic two-colour pictures losslessly under every arm and
        // leave nothing to compare (see [`force_screen`]).
        force_screen(Some(false));
        // Same reason, for the filter-intra candidate: it wins blocks under
        // BOTH arms of a mode ablation and so cancels the difference the
        // ablation measures (class `gate-recipe-confound`).
        force_filter_intra(Some(false));
        let mut points: Vec<(f64, f64)> = [110u8, 90, 70]
            .iter()
            .map(|&q| {
                let encoded =
                    encode_key_frame_with_modes_with_ctx(picture, q, 0.5, modes, fctx).unwrap();
                (
                    psnr(&encoded.reconstruction.y, picture.y.as_slice()),
                    (encoded.stream.len() as f64).log10(),
                )
            })
            .collect();
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        points
    }

    /// What the search is for: the same pictures cost less to code at the same
    /// fidelity than DC prediction alone can manage. A picture that runs one
    /// way saves far more than a busy one, which is the shape a working
    /// directional pair has.
    #[test]
    fn the_search_beats_dc_alone() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        for (name, picture, want) in [
            ("test card", test_card(160, 96), -0.05),
            ("stripes", stripes(160, 96, true), -0.40),
        ] {
            let saved = bd_rate(
                &ladder(&picture, &[DC_PRED], fctx),
                &ladder(&picture, &NON_DIRECTIONAL, fctx),
            );
            assert!(
                saved < want,
                "{name}: the search saved {:.1}% of the rate, wanted at least {:.1}%",
                saved * 100.0,
                -want * 100.0
            );
        }
    }

    /// The pictures a sweep is measured over: two synthetic ones, plus a frame
    /// from each clip named in `EC_AV1_CLIPS` as `path@skip`, colon separated.
    pub(super) fn sweep_pictures() -> Vec<(String, Picture)> {
        let mut pictures = vec![
            ("test card".to_string(), test_card(160, 96)),
            ("stripes".to_string(), stripes(160, 96, true)),
            ("diagonal".to_string(), diagonal(160, 96, true)),
        ];
        if let Ok(clips) = std::env::var("EC_AV1_CLIPS") {
            for entry in clips.split(':').filter(|e| !e.is_empty()) {
                let (path, skip) = entry.split_once('@').unwrap_or((entry, "0"));
                let name = path.rsplit('/').next().unwrap_or(path).to_string();
                pictures.push((name, clip_frame(path, skip, 640, 352)));
            }
        }
        pictures
    }

    /// What the six directional modes are worth, per picture, at whatever
    /// `LAMBDA_SCALE` currently is: the rate they save over the seven that read
    /// no further, and how often the search picks one.
    #[test]
    #[ignore = "a sweep, not a gate"]
    fn probe_directional() {
        let _knobs = crate::speed::knob_write();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        for (name, picture) in sweep_pictures() {
            let dc = ladder(&picture, &[DC_PRED], fctx);
            let flat = ladder(&picture, &NON_DIRECTIONAL, fctx);
            let all = ladder(&picture, &KEY_FRAME_MODES, fctx);
            let encoded = encode_key_frame_with_ctx(&picture, 90, 0.5, fctx).unwrap();
            let directional = encoded
                .modes
                .iter()
                .filter(|&&m| (3..=8).contains(&m))
                .count();
            // A picture whose ladders sit at different fidelities altogether
            // has no BD-rate to report -- see the flat-sample-window class.
            let overlap = |a: &[(f64, f64)], b: &[(f64, f64)]| {
                a[0].0.max(b[0].0) < a[a.len() - 1].0.min(b[b.len() - 1].0)
            };
            let against = |a: &[(f64, f64)], b: &[(f64, f64)]| {
                if overlap(a, b) {
                    format!("{:+.2}%", bd_rate(a, b) * 100.0)
                } else {
                    "no overlap".to_string()
                }
            };
            println!(
                "{name}: {} against the seven, {} against DC alone, {directional} of {} blocks directional",
                against(&flat, &all),
                against(&dc, &all),
                encoded.modes.len(),
            );
        }
    }

    /// A picture whose stripes run along a diagonal, one way or the other.
    /// This is to the directional modes what [`stripes`] is to the vertical and
    /// horizontal pair: a predictor that walked the edge in the wrong direction
    /// would answer the two pictures the same way round.
    pub(super) fn diagonal(width: usize, height: usize, down_right: bool) -> Picture {
        let mut picture = Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let along = if down_right { x + height - y } else { x + y };
                picture.y[y * width + x] = if (along / 6) % 2 == 0 { 40 } else { 210 };
            }
        }
        picture
    }

    /// The mode picked by the most blocks of a picture, ignoring the first
    /// block, which has no neighbours to tell the modes apart with.
    pub(super) fn favourite_mode(picture: &Picture, fctx: &crate::decode::FrameCtx) -> (u8, usize, usize) {
        // A mode ablation measures the MODE search: the palette would code
        // these synthetic two-colour pictures losslessly under every arm and
        // leave nothing to compare (see [`force_screen`]).
        force_screen(Some(false));
        let encoded = encode_key_frame_with_ctx(picture, 100, 0.5, fctx).unwrap();
        let blocks = &encoded.modes[1..];
        let mut counts = [0usize; 13];
        for &mode in blocks {
            counts[usize::from(mode)] += 1;
        }
        let (mode, count) = counts
            .iter()
            .enumerate()
            .max_by_key(|&(_, count)| *count)
            .expect("thirteen modes");
        (mode as u8, *count, blocks.len())
    }

    /// The search has to follow a diagonal the way it runs: stripes down and to
    /// the right are cheapest predicted at 135 degrees, stripes down and to the
    /// left at 45. A walk that stepped the wrong way along the edge, or read
    /// the above row where it should read the left column, would swap these.
    #[test]
    fn the_search_picks_the_diagonal_the_picture_runs() {
        let _knobs = crate::speed::knob_write();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        for (down_right, want) in [(true, D135_PRED), (false, D45_PRED)] {
            let picture = diagonal(160, 96, down_right);
            let (mode, count, blocks) = favourite_mode(&picture, fctx);
            assert_eq!(
                mode, want,
                "down_right={down_right}: {count} of {blocks} blocks picked mode {mode}"
            );
        }
    }

    /// What the directional modes are for: a picture that runs along a diagonal
    /// costs far less to code with them than the seven that read only their own
    /// edges can manage, and a picture that runs no particular way costs no
    /// more. The second half is the one that bites: a mode set the search
    /// cannot price is a mode set that loses rate on content that does not want
    /// it, which is what costing the mode symbol is for.
    #[test]
    fn the_diagonals_beat_the_modes_that_read_no_further() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        for (name, picture, want) in [
            ("down-right", diagonal(160, 96, true), -0.20),
            ("down-left", diagonal(160, 96, false), -0.20),
            ("test card", test_card(160, 96), 0.01),
        ] {
            let saved = bd_rate(
                &ladder(&picture, &NON_DIRECTIONAL, fctx),
                &ladder(&picture, &KEY_FRAME_MODES, fctx),
            );
            assert!(
                saved < want,
                "{name}: the directional modes saved {:.1}% of the rate, wanted better than {:.1}%",
                saved * 100.0,
                -want * 100.0
            );
        }
    }

    /// A mode the encoder cannot predict must be refused rather than coded as
    /// something else, and a search with nothing to choose from likewise.
    #[test]
    fn a_mode_the_encoder_cannot_predict_is_refused() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        let picture = test_card(64, 64);
        let message = encode_key_frame_with_modes_with_ctx(&picture, 100, 0.5, &[13], fctx)
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("intra mode 13"),
            "the refusal must name the mode, got {message}"
        );
        assert!(encode_key_frame_with_modes_with_ctx(&picture, 100, 0.5, &[], fctx).is_err());
    }

    /// The picture that comes back is the picture that went in, to within the
    /// quantizer. A prediction that read the wrong neighbours, or a block
    /// written into the wrong place, would still decode to what the encoder
    /// reconstructed — it is this gate that says the reconstruction is of the
    /// right picture.
    #[test]
    fn the_encoded_picture_is_the_one_that_went_in() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        let picture = test_card(160, 96);
        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        let luma = psnr(&encoded.reconstruction.y, &picture.y);
        assert!(luma > 36.0, "luma PSNR {luma} at q 100");
        for (plane, (got, want)) in [
            (encoded.reconstruction.u.as_slice(), picture.u.as_slice()),
            (encoded.reconstruction.v.as_slice(), picture.v.as_slice()),
        ]
        .iter()
        .enumerate()
        {
            let chroma = psnr(got, want);
            assert!(chroma > 40.0, "chroma plane {plane} PSNR {chroma} at q 100");
        }
    }

    /// A finer quantizer costs bits and buys fidelity, and a wider deadzone
    /// does the opposite. Both are monotone, which is what a rate-distortion
    /// loop above this will assume.
    #[test]
    fn fidelity_and_rate_move_with_the_quantizer() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        let picture = test_card(128, 128);
        let mut previous: Option<(usize, f64)> = None;
        for &q in &[70u8, 90, 110] {
            let encoded = encode_key_frame_with_ctx(&picture, q, 0.5, fctx).unwrap();
            let quality = psnr(&encoded.reconstruction.y, &picture.y);
            if let Some((bytes, better)) = previous {
                assert!(
                    encoded.stream.len() < bytes,
                    "q {q}: {} bytes",
                    encoded.stream.len()
                );
                assert!(quality < better, "q {q}: PSNR {quality}");
            }
            previous = Some((encoded.stream.len(), quality));
        }

        // The deadzone half of this claim lives in
        // `transform::tests::a_wider_deadzone_codes_fewer_coefficients` now:
        // with the rate-distortion pass ([`crate::tile::rdoq`]) deciding
        // every zeroing, the deadzone no longer moves a coded stream
        // monotonically (the i64 + rdoq merge read 318 bytes at 0.3 against
        // fewer at 0.5 on this 128x128 card), and that is the pass doing its
        // job, not a quantiser defect.

        // Bytes must not jump up across a q-context boundary: a wrong CDF
        // table for the far side would show up as a rate discontinuity here,
        // even though a coarser quantizer always codes no more than a finer
        // one on the same picture.
        //
        // corner-cut (lane-av1rd3): the bound is 5% rather than exact. Since
        // chroma searches its own mode, a coarser quantizer can pick a
        // *different* uv_mode on a block whose modes now cost nearly the
        // same, and the search prices that symbol against the static default
        // CDF while the writer codes it against an adapted one -- 311 -> 319
        // bytes at q 120->121 on this picture. The table this gate is really
        // watching would move the rate by far more than the mode churn does.
        // lane-av1tx widened it again, 5% -> 10%: under `TxMode::Select` the
        // same block can also change transform DEPTH between two neighbouring
        // quantizers (297 -> 315 bytes at q 120->121 here), and the depth
        // symbol is priced off the static CDF row 0 for the same reason the
        // uv_mode one is.
        // Ceiling: a q-context table wrong by under 10% would pass. Upgrade
        // path is pricing through the adapting CDF state (what
        // `predicted_coeff_bits_track_the_tile_the_writer_wrote` measures the
        // drift of), after which the exact bound comes back.
        for &(lo, hi) in &[(20u8, 21u8), (60, 61), (120, 121)] {
            let lo_bytes = encode_key_frame_with_ctx(&picture, lo, 0.5, fctx)
                .unwrap()
                .stream
                .len();
            let hi_bytes = encode_key_frame_with_ctx(&picture, hi, 0.5, fctx)
                .unwrap()
                .stream
                .len();
            assert!(
                hi_bytes as f64 <= lo_bytes as f64 * 1.10,
                "q {lo}->{hi} crosses a context boundary: {lo_bytes} -> {hi_bytes} bytes"
            );
        }
    }

    /// A flat picture is a flat stream: every block predicts its neighbours'
    /// average, which is the picture's own value, and codes nothing.
    #[test]
    fn a_flat_picture_costs_almost_nothing() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        let mut picture = Picture::grey(128, 128);
        picture.y.fill(97);
        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        // The first block has no neighbour and predicts 128, so it carries a
        // DC; every block after it predicts 97 and carries nothing.
        assert!(
            encoded.stream.len() < 100,
            "{} bytes for a flat picture",
            encoded.stream.len()
        );
        for (i, &s) in encoded.reconstruction.y.iter().enumerate().skip(32 * 128) {
            assert_eq!(s, 97, "sample {i} of a flat picture");
        }
    }

    /// The sizes the encoder refuses, refused for a reason rather than by
    /// panicking somewhere below. Sizes off the 32x32 block grid encode fine
    /// (see `a_frame_round_trips_at_its_own_size`) -- the true frame edge
    /// lands past the halfway point of whichever block it falls in, so
    /// `PARTITION_NONE` or a single gathered split flag still says everything
    /// the spec needs. lane-av1-rect r7 wired the 8x8-leaf path for a 16x16
    /// leaf straddling on exactly one axis (e.g. 40x32), and lane-av1rect the
    /// both-axes cut (e.g. 40x40, both dims mod 32 == 8), which spec 5.11.4
    /// codes with NO partition symbol at all -- `PARTITION_SPLIT` inferred,
    /// down to the in-frame 8x8s, no rectangular transform anywhere (the old
    /// refusal here claimed otherwise and was wrong; see
    /// `crate::stream::tests::a_sweep_of_doubly_straddling_sizes_round_trips_
    /// through_ffmpeg`). No size refusal is left: an 8x8 leaf can never
    /// straddle either, because `MiCols`/`MiRows` are always EVEN (spec
    /// 5.9.5 `compute_image_size`: `2 * ((frame_width + 7) >> 3)`), so the
    /// true edge always lands on an 8-luma-sample boundary in mi terms and a
    /// 4x4 leaf -- which neither writer has -- is never asked for. What is
    /// still refused is a malformed picture.
    #[test]
    fn a_picture_off_the_block_grid_encodes_and_a_malformed_one_is_refused() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        encode_key_frame_with_ctx(&Picture::grey(40, 40), 100, 0.5, fctx)
            .expect("40x40 cuts a 16x16 leaf on both axes, which is an inferred split");
        encode_key_frame_with_ctx(&Picture::grey(36, 40), 100, 0.5, fctx)
            .expect("36 luma columns still round up to an even MiCols");

        let mut short = Picture::grey(64, 64);
        short.u.truncate(10);
        assert!(encode_key_frame_with_ctx(&short, 100, 0.5, fctx).is_err());
    }

    /// 40x32: cols mod 32 == 8, so the true edge cuts a 16x16 leaf's own
    /// column half only (`has_half` false on cols, true on rows) -- exactly
    /// the single-axis straddle lane-av1-rect r7 wires through
    /// `crate::tile::write_leaf8`'s 8x8 leaves. Not yet decoder-clean:
    /// dav1d rejects the stream ("Invalid data found"). r7's falsification
    /// candidate (`partition_w8`'s ctx reading the enclosing 16x16 slot) is
    /// FALSIFIED by r8's rng trace against a debug `aomdec`: libaom's own
    /// `partition_plane_context` masks at `bsl = 0` for `BLOCK_8X8`, and every
    /// `partition_context_lookup` entry this writer ever produces (no 4x4
    /// splits exist) has bit 0 clear, so the ctx is provably 0 for every
    /// leaf regardless of neighbour state -- our writer already agreed with
    /// the decoder here (`ctx=0` both sides). r8 still ported the mi-precise
    /// tracking (`Neighbours::{above,left}_side_mi`, `partition_ctx_mi`) as a
    /// spec-accurate fix in its own right (it matters once 4x4 exists), and
    /// it does not regress the gate. r11 tell-for-tell traced two further,
    /// real bugs and fixed both: (1) `write_leaf8`'s intra-mode context read
    /// the *enclosing* 16x16 slot's stale `above_mode`/`left_mode` for both
    /// leaves, when the second leaf's true above (or left) neighbour is the
    /// first leaf itself -- fixed by threading the first leaf's mode into the
    /// second leaf's context, mi-precise, like `record_mi` already does for
    /// coefficients; (2) `TxbSet::Luma8`'s transform-type table was
    /// `INTRA_TX_TYPE_SET1_8` (`TX_SET_INTRA_1`, 7-way, correct only when
    /// `reduced_tx_set` is false), but this crate's key frames set
    /// `reduced_tx_set: true`, which per `get_tx_set` (spec 5.11.48) puts
    /// *every* intra size up to 16x16 -- not only 16x16 -- on `TX_SET_INTRA_2`
    /// (5-way); the wrong cardinality read enough of the stream as `TX_TYPE`
    /// symbol bits to desync everything after it. Fixed with a new
    /// `INTRA_TX_TYPE_SET2_8` (libaom's flat default for that exact
    /// eset/size slot). Both fixes are provably correct against libaom
    /// source and each closed part of the gap (leaf0's `eob` went from a
    /// desynced 36 to a near-miss 3, writer's real eob is 7), but a residual
    /// divergence remained in the `EOB_PT` symbol's decoded value despite an
    /// aligned bit cost. r12 traced that residual with a fresh dual-trace
    /// (do not reuse r11's committed logs, which predate its own fixes) and
    /// found two more real bugs, both now fixed: (3) `write_leaf8` hardcoded
    /// `cfl: false` for `write_intra_mode`'s chroma-mode CDF choice, but
    /// `is_cfl_allowed` (spec 5.11.5) is true for any block `<= 32x32` --
    /// every other `write_block` caller at 16x16 and up already passes
    /// `true`; the narrower `uv_mode_no_cfl` alphabet is one symbol short of
    /// `uv_mode_cfl`, so even an identical `DC_PRED` decision consumed a
    /// different amount of arithmetic range and derailed everything after
    /// it. (4) `record()`'s `above_mode`/`left_mode` write is a no-op at an
    /// 8x8 leaf's own side (`side / SUB == 0` for `SUB` = 16), and
    /// `write_leaf8` called only `record_mi` (the coefficient-context half),
    /// never updating those coarse arrays itself -- so a *second* straddling
    /// 16x16 quadrant stacked above or left of the first (never covered by
    /// r11's same-call `prev_leaf` override, which only threads state
    /// between the two leaves of one shared quadrant) read whatever stale
    /// mode sat in that slot before either leaf ran. Fixed by writing the
    /// leaf's own mode into `above_mode[c]`/`left_mode[r]` directly. Fixing
    /// (3) alone shrank the tell drift after leaf(0,8) from a decoder
    /// `eob_pt=3` (wrong) vs the writer's real `eob_pt=4` down to a `tell`
    /// offset of a single unit; fixing (3)+(4) together still leaves a
    /// residual divergence one leaf further in -- the decoder now reads
    /// leaf(2,8)'s own luma coefficient block as `txs_ctx=1` (a 32-sample
    /// transform) where the writer intends `TX_8X8` (`txs_ctx=2`), meaning
    /// something *before* this leaf's own `EC_TXBSKIP`/`EC_TXTYPE` still
    /// desyncs the stream by exactly the width of one earlier symbol -- not
    /// yet isolated to a single named symbol. r15 found the last bug: the
    /// header fix from r14 explained the rng skew fully, and the remaining
    /// desync (still past `mi_row=4`) was `write_leaf8`'s (4) fix itself --
    /// its `above_mode[c]`/`left_mode[r]` forced write ran *inside* the leaf
    /// loop, once per leaf, so the first leaf's write clobbered the true
    /// external neighbour mode the second leaf of the *same* straddling
    /// quadrant still needed for its non-adjacency axis (a column-straddle
    /// case: two leaves share one `outer_at` column index, so leaf 1's
    /// write to `left_mode[r]` overwrote the real western neighbour before
    /// leaf 2 read it). Fixed by moving the write out of `write_leaf8` and
    /// doing it once, after the whole leaf loop, from the last (bottom/
    /// right-most) leaf's mode. The stream now round-trips through ffmpeg.
    #[test]
    fn a_single_axis_straddle_round_trips_through_ffmpeg() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        if !have_ffmpeg() {
            eprintln!("SKIP a_single_axis_straddle_round_trips_through_ffmpeg: no ffmpeg");
            return;
        }
        let (width, height) = (40usize, 32usize);
        let picture = test_card(width, height);
        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        assert_eq!(
            (encoded.reconstruction.width, encoded.reconstruction.height),
            (width, height)
        );
        if let Ok(path) = std::env::var("EC_AV1_DUMP") {
            std::fs::write(&path, &encoded.stream).expect("dump the raw stream");
        }
        let decoded = ffmpeg_decode(&encoded.stream, width, height);
        assert_eq!(decoded.y, encoded.reconstruction.y, "luma");
        assert_eq!(decoded.u, encoded.reconstruction.u, "U");
        assert_eq!(decoded.v, encoded.reconstruction.v, "V");
    }

    /// A frame of real video, rather than a picture built to be easy.
    ///
    /// `EC_AV1_CLIP` names the clip and `EC_AV1_CLIP_SKIP` how far into it to
    /// seek; ffmpeg decodes one frame, crops it to the block grid, and the
    /// encoder's reconstruction has to survive the same equality gate as the
    /// synthetic pictures — real video reaches contexts a test card does not.
    #[test]
    fn a_frame_of_real_video_decodes_to_what_the_encoder_reconstructed() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        let Ok(clip) = std::env::var("EC_AV1_CLIP") else {
            eprintln!(
                "SKIP a_frame_of_real_video_decodes_to_what_the_encoder_reconstructed: \
                 set EC_AV1_CLIP to a clip"
            );
            return;
        };
        if !have_ffmpeg() {
            eprintln!(
                "SKIP a_frame_of_real_video_decodes_to_what_the_encoder_reconstructed: no ffmpeg"
            );
            return;
        }
        let skip = std::env::var("EC_AV1_CLIP_SKIP").unwrap_or_else(|_| "0".into());
        // A 4:2:0 frame cropped to whole 32x32 blocks, in a size that keeps the
        // test quick while still spanning many superblocks.
        let (width, height) = (640usize, 352usize);
        let picture = clip_frame(&clip, &skip, width, height);

        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        let decoded = ffmpeg_decode(&encoded.stream, width, height);
        assert_eq!(decoded.y, encoded.reconstruction.y, "luma");
        assert_eq!(decoded.u, encoded.reconstruction.u, "U");
        assert_eq!(decoded.v, encoded.reconstruction.v, "V");
        eprintln!(
            "{} bytes, luma PSNR {:.2} dB",
            encoded.stream.len(),
            psnr(&decoded.y, &picture.y)
        );
    }

    /// [`test_card`], panned `shift` samples to the right (wrapping), so a
    /// sequence of these is a translation a motion search can actually find
    /// — the content [`test_card`] itself draws, not a fresh pattern, so an
    /// inter frame's rate against the key frame's is measuring the same
    /// picture moving, not two different pictures.
    pub(super) fn panned_test_card(width: usize, height: usize, shift: i64) -> Picture {
        let mut picture = Picture::grey(width, height);
        for y in 0..height {
            for x in 0..width {
                let sx = (x as i64 - shift).rem_euclid(width as i64) as f64;
                let gradient = sx * 200.0 / width as f64;
                let ripple = 30.0
                    * (sx * std::f64::consts::PI / 23.0).sin()
                    * (y as f64 * std::f64::consts::PI / 37.0).cos();
                let edge = if sx > (width * 3 / 7) as f64 && y > height / 3 {
                    40.0
                } else {
                    0.0
                };
                picture.y[y * width + x] =
                    (20.0 + gradient + ripple + edge).clamp(0.0, 255.0) as u16;
            }
        }
        for y in 0..height / 2 {
            for x in 0..width / 2 {
                let sx = (x as i64 - shift / 2).rem_euclid((width / 2) as i64) as usize;
                let i = y * width / 2 + x;
                picture.u[i] = (100 + (sx * 60 / (width / 2))) as u16;
                picture.v[i] = (200 - (y * 80 / (height / 2))) as u16;
            }
        }
        picture
    }

    /// Decodes `frames` concatenated 4:2:0 frames out of one AV1 OBU stream.
    pub(super) fn ffmpeg_decode_sequence(
        stream: &[u8],
        width: usize,
        height: usize,
        frames: usize,
    ) -> Vec<Picture> {
        let out = run_with_stdin(
            Command::new("ffmpeg").args([
                "-v", "error", "-f", "obu", "-i", "-", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-",
            ]),
            stream,
        );
        assert!(
            out.status.success(),
            "ffmpeg refused the stream: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (luma, chroma) = (width * height, width * height / 4);
        let frame_bytes = luma + 2 * chroma;
        assert_eq!(
            out.stdout.len(),
            frame_bytes * frames,
            "expected {frames} 4:2:0 frames, ffmpeg said: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        (0..frames)
            .map(|i| {
                let base = i * frame_bytes;
                Picture {
                    width,
                    height,
                    y: out.stdout[base..base + luma]
                        .iter()
                        .map(|&v| u16::from(v))
                        .collect(),
                    u: out.stdout[base + luma..base + luma + chroma]
                        .iter()
                        .map(|&v| u16::from(v))
                        .collect(),
                    v: out.stdout[base + luma + chroma..base + frame_bytes]
                        .iter()
                        .map(|&v| u16::from(v))
                        .collect(),
                }
            })
            .collect()
    }

    /// The same claim [`ffmpeg_decodes_exactly_what_the_encoder_reconstructed`]
    /// makes for a key frame, extended down the reference chain: each inter
    /// frame's reconstruction predicts from the previous frame's own decoded
    /// reconstruction, so a single sample of drift anywhere would propagate
    /// into every frame after it -- which is why every frame is checked, not
    /// just the last one, and why this is an equality and not a tolerance.
    #[test]
    fn every_frame_of_a_sequence_decodes_to_what_the_encoder_reconstructed() {
        let _knobs = crate::speed::knob_read();
        let fctx = &crate::decode::FrameCtx::for_encoder();
        if !have_ffmpeg() {
            eprintln!(
                "SKIP every_frame_of_a_sequence_decodes_to_what_the_encoder_reconstructed: no ffmpeg"
            );
            return;
        }
        let (width, height) = (128usize, 64usize);
        let pictures: Vec<Picture> = (0..5)
            .map(|i| panned_test_card(width, height, i * 3))
            .collect();
        let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
        assert_eq!(encoded.frames.len(), 5);

        let decoded = ffmpeg_decode_sequence(&encoded.stream, width, height, 5);
        assert_eq!(
            encoded.frames.len(),
            decoded.len(),
            "frame count: ours vs ffmpeg"
        );
        for (i, (frame, dec)) in encoded.frames.iter().zip(&decoded).enumerate() {
            assert_eq!(dec.y, frame.reconstruction.y, "frame {i}: luma");
            assert_eq!(dec.u, frame.reconstruction.u, "frame {i}: U");
            assert_eq!(dec.v, frame.reconstruction.v, "frame {i}: V");
        }

        eprintln!("frame  bytes  luma PSNR (dB)  inter share");
        for (i, frame) in encoded.frames.iter().enumerate() {
            eprintln!(
                "{i:5}  {:5}  {:14.2}  {:11.2}",
                frame.stream.len(),
                psnr(&frame.reconstruction.y, &pictures[i].y),
                frame.inter_block_share
            );
        }
    }

    // The same sequence through OUR OWN decoder, sample-exact against the
    // encoder's reconstruction. ffmpeg stays the bitstream oracle
    // ([[shared-oracle-blindness]]) -- but a stream ffmpeg REFUSES yields no
    // divergence point at all, while our decoder can be traced symbol by
    // symbol (`EC_TRACE_MODE_STEP`) on the very stream that broke. This is
    // the instrument the inter `TxMode::Select` hunt lacked; on failure it
    // names the first differing frame, plane and position.
    // lane-sb128b: the writer used to price a `NEWMV` residual against
    // `stack.pred_mv` whenever the SEARCH chose a `ref_mv_idx` the
    // write-time stack could not carry, while the drl symbols signalled the
    // clamped index -- so the decoder derived a DIFFERENT motion vector with
    // the entropy stream still in sync, and the drift only surfaced frames
    // later as a drl desync (on film B, as the encoder's own trial decode
    // refusing a reference slot).
    //
    // The clamp needs a stack that shrinks between search and write, which
    // is what a 128x128 superblock's visit order does; the counter is the
    // point (class `gate-blind-to-feature`): a green round trip on a clip
    // that never clamps proves nothing.
    //
    // `#[ignore]`: [`force_sb128`] moves a process-global.
    //
    // The drl-clamp cell this gate's counter used to gate is no longer gated
    // here: it is covered by
    // [`every_drl_index_the_new_mv_pricer_offers_is_one_the_writer_can_signal`],
    // which enumerates the pricer's offer set against the writer's rule (see
    // its doc). The clamp needs a stack that shrinks between search and
    // write, which is what a 128x128 superblock's visit order would do -- but
    // the pricer never offers an index this tree's `write_drl_idx` cannot
    // signal, so no clip can provoke it (lane-av1recipehunt: 153 attempts, 0;
    // widening the pricer to `0..5` fires the counter immediately, reverting
    // silences it). `drl_clamp_hits` is kept as the tripwire for that fix.

    /// lane-b128: the 128x128 `PARTITION_NONE` root -- one block, one mode
    /// and one motion vector where the four 64x64 roots below spend four of
    /// each (cen3's L1). The counter is the gate on the fixture (class
    /// `gate-blind-to-feature`): a green round trip on a clip that never
    /// codes a 128x128 block proves nothing. RED before the search existed
    /// (zero such blocks coded, `tile::write_sb128_root` wrote SPLIT at every
    /// root by construction).
    ///
    /// Both readers: our own decoder sample-exact against the encoder's
    /// reconstruction (a writer/reader desync surfaces as a pixel miss or a
    /// refusal here, not in the BD gate) AND ffmpeg as the bitstream oracle.
    ///
    /// `#[ignore]`: [`force_sb128`] moves a process-global.
    ///
    /// Runs only inside `superblock_size_global_suite`.
    fn a_128_superblock_clip_whose_root_search_codes_128x128_blocks_decodes_exact() {
        let _knobs = crate::speed::knob_write();
        if !have_ffmpeg() {
            eprintln!("SKIP: no ffmpeg");
            return;
        }
        force_sb128(true);
        let dir = std::env::temp_dir().join(format!("ec-av1-b128-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("smpte-1280x768.y4m");
        // A slow horizontal pan over a still pattern: whole 128x128 regions
        // share one motion vector, which is exactly what a 128 root can code
        // as one block and four 64 roots cannot.
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi"])
            .args(["-i", "smptebars=size=1600x768:rate=24"])
            .args(["-vf", "crop=1280:768:'min(n*2,300)':0", "-frames:v", "8"])
            .args(["-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .expect("ffmpeg failed to run")
            .success();
        assert!(ok, "ffmpeg could not write the synthetic clip");
        let (width, height) = (1280usize, 768usize);
        let pictures = crate::probe::source(clip.to_str().unwrap(), "0", "null", width, height, 8);
        let _ = take_b128_none_hits();
        let _ = crate::tile::take_sb128_none_hits();
        let encoded = encode_sequence(&pictures, 150, 0.5).unwrap();
        let (won, coded) = (take_b128_none_hits(), crate::tile::take_sb128_none_hits());
        eprintln!("128x128 PARTITION_NONE blocks: search won {won}, writer coded {coded}");
        assert!(
            coded > 0,
            "no 128x128 block was coded on this clip, so it cannot witness the root search",
        );
        assert_eq!(
            won, coded,
            "every 128x128 root the search took must be coded as one"
        );
        let decoded = crate::stream::decode_stream(&encoded.stream).expect("our decoder");
        assert_eq!(decoded.len(), encoded.frames.len(), "frame count");
        for (i, (frame, dec)) in encoded.frames.iter().zip(&decoded).enumerate() {
            let recon = &frame.reconstruction;
            for (plane, got, want) in [
                ("luma", &dec.y, &recon.y),
                ("U", &dec.u, &recon.u),
                ("V", &dec.v, &recon.v),
            ] {
                let differ = got.iter().zip(want.iter()).filter(|(a, b)| a != b).count();
                assert_eq!(
                    differ,
                    0,
                    "frame {i}: {plane} -- {differ} of {} samples differ from the encoder's \
                     own reconstruction",
                    got.len()
                );
                assert_eq!(
                    got.len(),
                    want.len(),
                    "frame {i}: {plane} is {} samples, the reconstruction's is {}",
                    got.len(),
                    want.len()
                );
            }
        }
        let ff = ffmpeg_decode_sequence(&encoded.stream, width, height, pictures.len());
        assert_eq!(
            encoded.frames.len(),
            ff.len(),
            "frame count: ours vs ffmpeg"
        );
        for (i, (frame, dec)) in encoded.frames.iter().zip(&ff).enumerate() {
            assert_eq!(dec.y, frame.reconstruction.y, "frame {i}: ffmpeg luma");
            assert_eq!(dec.u, frame.reconstruction.u, "frame {i}: ffmpeg U");
            assert_eq!(dec.v, frame.reconstruction.v, "frame {i}: ffmpeg V");
        }
    }

    /// lane-b128r: the 128 root's own RESIDUAL arm -- four TX_64X64 luma
    /// units interleaved with a TX_32X32 chroma pair per 64x64 mu chunk. The
    /// same pan the skip-only witness above uses, with the arm FORCED
    /// ([`force_b128_residual`]): on synthetic bars the RD picks the residual
    /// about once per frame, which witnesses the shape but not every context
    /// it reads, and forcing codes all 420 roots of the clip through it. RED
    /// before this lane by construction (`write_inter_block_128` refused
    /// every non-skip 128 block, so no such block existed to decode), and RED
    /// again on the first draft of the writer, whose neighbour band recorded
    /// `skip = true` for a block that carries a residual -- the next root's
    /// `skip` symbol then took CDF row 1 where the reader takes row 0.
    ///
    /// lane-b128res: the fixture's q moved 150 -> 60. The forced arm replaces
    /// the skip arm's cost unconditionally (that is what makes the writer see
    /// the residual), and the cost model moved under it since lane-b128r --
    /// lane-rectdq made every non-skipped root pay its delta_q group and the
    /// pyramid re-prices the leaves -- so at q=150, then q=90, the NONE root
    /// lost its partition at ALL 420 roots and the writer coded none. q=60
    /// puts 52 written blocks back on this clip; the `coded > 0` assert below
    /// is the guard that catches the next such drift.
    ///
    /// Runs only inside `superblock_size_global_suite`.
    fn a_128_root_block_with_a_real_residual_decodes_exact_through_both_decoders() {
        let _knobs = crate::speed::knob_write();
        if !have_ffmpeg() {
            eprintln!("SKIP: no ffmpeg");
            return;
        }
        force_sb128(true);
        let dir = std::env::temp_dir().join(format!("ec-av1-b128r-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("smpte-pan-1280x768.y4m");
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi"])
            .args(["-i", "smptebars=size=1600x768:rate=24"])
            .args(["-vf", "crop=1280:768:'min(n*2,300)':0", "-frames:v", "8"])
            .args(["-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .expect("ffmpeg failed to run")
            .success();
        assert!(ok, "ffmpeg could not write the synthetic clip");
        let (width, height) = (1280usize, 768usize);
        let pictures = crate::probe::source(clip.to_str().unwrap(), "0", "null", width, height, 8);
        let _ = take_b128_residual_hits();
        let _ = crate::tile::take_sb128_residual_hits();
        force_b128_residual(true);
        let encoded = encode_sequence(&pictures, 60, 0.5).unwrap();
        let (won, coded) = (
            take_b128_residual_hits(),
            crate::tile::take_sb128_residual_hits(),
        );
        eprintln!("128x128 blocks WITH a residual: search won {won}, writer coded {coded}");
        assert!(
            coded > 0,
            "no 128x128 block carried a residual on this clip, so it cannot witness the arm",
        );
        let decoded = crate::stream::decode_stream(&encoded.stream).expect("our decoder");
        assert_eq!(decoded.len(), encoded.frames.len(), "frame count");
        for (i, (frame, dec)) in encoded.frames.iter().zip(&decoded).enumerate() {
            let recon = &frame.reconstruction;
            for (plane, got, want) in [
                ("luma", &dec.y, &recon.y),
                ("U", &dec.u, &recon.u),
                ("V", &dec.v, &recon.v),
            ] {
                let differ = got.iter().zip(want.iter()).filter(|(a, b)| a != b).count();
                assert_eq!(
                    differ,
                    0,
                    "frame {i}: {plane} -- {differ} of {} samples differ from the encoder's \
                     own reconstruction",
                    got.len()
                );
                assert_eq!(
                    got.len(),
                    want.len(),
                    "frame {i}: {plane} is {} samples, the reconstruction's is {}",
                    got.len(),
                    want.len()
                );
            }
        }
        let ff = ffmpeg_decode_sequence(&encoded.stream, width, height, pictures.len());
        assert_eq!(
            encoded.frames.len(),
            ff.len(),
            "frame count: ours vs ffmpeg"
        );
        for (i, (frame, dec)) in encoded.frames.iter().zip(&ff).enumerate() {
            assert_eq!(dec.y, frame.reconstruction.y, "frame {i}: ffmpeg luma");
            assert_eq!(dec.u, frame.reconstruction.u, "frame {i}: ffmpeg U");
            assert_eq!(dec.v, frame.reconstruction.v, "frame {i}: ffmpeg V");
        }
    }

    /// lane-edge128: a 128x128 root block WITH a residual on a clip whose
    /// CDEF search codes a per-64x64 `cdef_idx` list. `read_cdef` (spec
    /// 5.11.56) codes ONE literal for the block that first reaches a unit and
    /// copies it over every unit the block SPANS, so the four units of a 128
    /// root share one index -- while the encoder's CDEF search prices a
    /// preset per unit and its filter replay applied all four. Only the first
    /// reached the stream, so every decoder filtered the other three with the
    /// origin's strengths and the encoder's reconstruction (the picture the
    /// next frame predicts from) was a filter apart from what a decoder
    /// produces. RED before this lane on the native BD gate's bars 2160p row
    /// (`EC_AV1_B128RES=1`, q=90, frame 11: 8 luma and 702 chroma samples,
    /// the rows above the 128 superblock's own edge); the stream itself was
    /// always right -- ffmpeg and our own decoder agreed with each other and
    /// disagreed with the encoder.
    ///
    /// `take_cdef_covered_units` is the gate on the fixture (class
    /// `gate-blind-to-feature`): a clip whose CDEF picks one strength pair
    /// for the whole frame codes no literal at all and witnesses nothing.
    ///
    /// Runs only inside `superblock_size_global_suite`.
    fn a_128_root_residual_block_under_a_per_unit_cdef_list_decodes_exact() {
        let _knobs = crate::speed::knob_write();
        if !have_ffmpeg() {
            eprintln!("SKIP: no ffmpeg");
            return;
        }
        force_sb128(true);
        let dir = std::env::temp_dir().join(format!("ec-av1-edge128-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("testsrc2-1280x768.y4m");
        // Detailed synthetic content: its CDEF search spends more than one
        // strength pair, which is what puts a `cdef_idx` literal in the tile.
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi"])
            .args(["-i", "testsrc2=size=1280x768:rate=24"])
            .args(["-frames:v", "12", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .expect("ffmpeg failed to run")
            .success();
        assert!(ok, "ffmpeg could not write the synthetic clip");
        let (width, height) = (1280usize, 768usize);
        let pictures = crate::probe::source(clip.to_str().unwrap(), "0", "null", width, height, 12);
        force_b128_root(true);
        force_b128_residual(true);
        let _ = crate::tile::take_sb128_residual_hits();
        let _ = crate::filter_search::take_cdef_covered_units();
        let encoded = encode_sequence(&pictures, 90, 0.5).unwrap();
        let coded = crate::tile::take_sb128_residual_hits();
        let covered = crate::filter_search::take_cdef_covered_units();
        eprintln!(
            "128x128 blocks WITH a residual: writer coded {coded}; 64x64 CDEF units taking \
             another unit's literal: {covered}"
        );
        assert!(
            coded > 0,
            "no 128x128 block carried a residual on this clip"
        );
        assert!(
            covered > 0,
            "no 64x64 unit took a non-zero preset from another unit's literal, so this clip \
             cannot witness the rule",
        );
        let decoded = crate::stream::decode_stream(&encoded.stream).expect("our decoder");
        assert_eq!(decoded.len(), encoded.frames.len(), "frame count");
        for (i, (frame, dec)) in encoded.frames.iter().zip(&decoded).enumerate() {
            for (plane, got, want) in [
                ("luma", &dec.y, &frame.reconstruction.y),
                ("U", &dec.u, &frame.reconstruction.u),
                ("V", &dec.v, &frame.reconstruction.v),
            ] {
                let differ = got.iter().zip(want.iter()).filter(|(a, b)| a != b).count();
                assert_eq!(differ, 0, "frame {i}: {plane} -- {differ} samples differ");
                assert_eq!(
                    got.len(),
                    want.len(),
                    "frame {i}: {plane} is {} samples, the reconstruction's is {}",
                    got.len(),
                    want.len()
                );
            }
        }
        let ff = ffmpeg_decode_sequence(&encoded.stream, width, height, pictures.len());
        assert_eq!(
            encoded.frames.len(),
            ff.len(),
            "frame count: ours vs ffmpeg"
        );
        for (i, (frame, dec)) in encoded.frames.iter().zip(&ff).enumerate() {
            assert_eq!(dec.y, frame.reconstruction.y, "frame {i}: ffmpeg luma");
            assert_eq!(dec.u, frame.reconstruction.u, "frame {i}: ffmpeg U");
            assert_eq!(dec.v, frame.reconstruction.v, "frame {i}: ffmpeg V");
        }
    }

    /// lane-rectres: a `PARTITION_HORZ` 128 root whose two 128x64 halves
    /// CARRY A RESIDUAL -- the mu-chunk shape (one TX_64X64 luma unit plus a
    /// TX_32X32 chroma pair per chunk) `write_inter_block_128_rect` now
    /// codes under `EC_AV1_RECTRES`, at the footprint the decoder's own
    /// mu-chunk loop reads. The arm is FORCED ([`force_b128_rectres`], the
    /// rect partition with [`force_b128_rect`]) so every inter root of the
    /// clip goes through it; the `coded > 0` assert is the guard that catches
    /// the next fixture drift (the whole-128 twin of this witness went RED
    /// twice that way).
    ///
    /// Runs only inside `superblock_size_global_suite`.
    fn a_rect128_half_with_a_real_residual_decodes_exact_through_both_decoders() {
        let _knobs = crate::speed::knob_write();
        if !have_ffmpeg() {
            eprintln!("SKIP: no ffmpeg");
            return;
        }
        force_sb128(true);
        let dir = std::env::temp_dir().join(format!("ec-av1-rectres-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("smpte-pan-1280x768.y4m");
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi"])
            .args(["-i", "smptebars=size=1600x768:rate=24"])
            .args(["-vf", "crop=1280:768:'min(n*2,300)':0", "-frames:v", "8"])
            .args(["-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .expect("ffmpeg failed to run")
            .success();
        assert!(ok, "ffmpeg could not write the synthetic clip");
        let (width, height) = (1280usize, 768usize);
        let pictures = crate::probe::source(clip.to_str().unwrap(), "0", "null", width, height, 8);
        force_b128_rect(1);
        force_b128_rectres(true);
        let _ = take_b128_rectres_hits();
        let _ = crate::tile::take_sb128_rectres_hits();
        let encoded = encode_sequence(&pictures, 60, 0.5).unwrap();
        let (won, coded) = (
            take_b128_rectres_hits(),
            crate::tile::take_sb128_rectres_hits(),
        );
        eprintln!("rect halves WITH a residual: search won {won}, writer coded {coded}");
        assert!(
            coded > 0,
            "no 128x64 half carried a residual on this clip, so it cannot witness the arm",
        );
        let decoded = crate::stream::decode_stream(&encoded.stream).expect("our decoder");
        assert_eq!(decoded.len(), encoded.frames.len(), "frame count");
        for (i, (frame, dec)) in encoded.frames.iter().zip(&decoded).enumerate() {
            let recon = &frame.reconstruction;
            for (plane, got, want) in [
                ("luma", &dec.y, &recon.y),
                ("U", &dec.u, &recon.u),
                ("V", &dec.v, &recon.v),
            ] {
                let differ = got.iter().zip(want.iter()).filter(|(a, b)| a != b).count();
                assert_eq!(
                    differ,
                    0,
                    "frame {i}: {plane} -- {differ} of {} samples differ from the encoder's \
                     own reconstruction",
                    got.len()
                );
                assert_eq!(
                    got.len(),
                    want.len(),
                    "frame {i}: {plane} is {} samples, the reconstruction's is {}",
                    got.len(),
                    want.len()
                );
            }
        }
        let ff = ffmpeg_decode_sequence(&encoded.stream, width, height, pictures.len());
        assert_eq!(
            encoded.frames.len(),
            ff.len(),
            "frame count: ours vs ffmpeg"
        );
        for (i, (frame, dec)) in encoded.frames.iter().zip(&ff).enumerate() {
            assert_eq!(dec.y, frame.reconstruction.y, "frame {i}: ffmpeg luma");
            assert_eq!(dec.u, frame.reconstruction.u, "frame {i}: ffmpeg U");
            assert_eq!(dec.v, frame.reconstruction.v, "frame {i}: ffmpeg V");
        }
    }

    /// lane-rectres: an AB shape (`PARTITION_HORZ_A` forced) whose THREE
    /// pieces carry residuals -- the 128x64 half through the two-chunk arm,
    /// the two 64x64 squares through the one-chunk arm (the shape libaom's
    /// own AB partitions earn their keep with). testsrc2 at 12 frames, the
    /// shipped AB witness's own recipe: an 8-frame clip parks an ARF whose
    /// reference picture the GOP has not produced yet, which refuses before
    /// any AB root is coded.
    ///
    /// Runs only inside `superblock_size_global_suite`.
    fn an_ab128_piece_with_a_real_residual_decodes_exact_through_both_decoders() {
        let _knobs = crate::speed::knob_write();
        if !have_ffmpeg() {
            eprintln!("SKIP: no ffmpeg");
            return;
        }
        force_sb128(true);
        let dir = std::env::temp_dir().join(format!("ec-av1-rectres-ab-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("testsrc2-1280x768.y4m");
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi"])
            .args(["-i", "testsrc2=size=1280x768:rate=24"])
            .args(["-frames:v", "12", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .expect("ffmpeg failed to run")
            .success();
        assert!(ok, "ffmpeg could not write the synthetic clip");
        let (width, height) = (1280usize, 768usize);
        let pictures = crate::probe::source(clip.to_str().unwrap(), "0", "null", width, height, 12);
        force_b128_ab(4);
        force_b128_rectres(true);
        let _ = take_b128_rectres_hits();
        let _ = crate::tile::take_sb128_rectres_hits();
        let encoded = encode_sequence(&pictures, 60, 0.5).unwrap();
        let (won, coded) = (
            take_b128_rectres_hits(),
            crate::tile::take_sb128_rectres_hits(),
        );
        eprintln!("AB pieces WITH a residual: search won {won}, writer coded {coded}");
        assert!(
            coded > 0,
            "no AB piece carried a residual on this clip, so it cannot witness the arm",
        );
        let decoded = crate::stream::decode_stream(&encoded.stream).expect("our decoder");
        assert_eq!(decoded.len(), encoded.frames.len(), "frame count");
        for (i, (frame, dec)) in encoded.frames.iter().zip(&decoded).enumerate() {
            let recon = &frame.reconstruction;
            for (plane, got, want) in [
                ("luma", &dec.y, &recon.y),
                ("U", &dec.u, &recon.u),
                ("V", &dec.v, &recon.v),
            ] {
                let differ = got.iter().zip(want.iter()).filter(|(a, b)| a != b).count();
                assert_eq!(
                    differ,
                    0,
                    "frame {i}: {plane} -- {differ} of {} samples differ from the encoder's \
                     own reconstruction",
                    got.len()
                );
                assert_eq!(
                    got.len(),
                    want.len(),
                    "frame {i}: {plane} is {} samples, the reconstruction's is {}",
                    got.len(),
                    want.len()
                );
            }
        }
        let ff = ffmpeg_decode_sequence(&encoded.stream, width, height, pictures.len());
        assert_eq!(
            encoded.frames.len(),
            ff.len(),
            "frame count: ours vs ffmpeg"
        );
        for (i, (frame, dec)) in encoded.frames.iter().zip(&ff).enumerate() {
            assert_eq!(dec.y, frame.reconstruction.y, "frame {i}: ffmpeg luma");
            assert_eq!(dec.u, frame.reconstruction.u, "frame {i}: ffmpeg U");
            assert_eq!(dec.v, frame.reconstruction.v, "frame {i}: ffmpeg V");
        }
    }

    /// lane-rectres, lane-edge128's interaction on the rect shapes: a HORZ
    /// half WITH a residual under a clip whose CDEF search codes a
    /// per-64x64 `cdef_idx` list. The half codes ONE literal at its origin
    /// and the decoder stamps BOTH units it spans with it, so the owner map
    /// ([`crate::tile::cdef_unit_owner`], now extended past Whole128) has to
    /// collapse the pair -- exactly the bug class that broke bars 2160p at
    /// the whole-128 root. RED before this lane by construction: no rect
    /// piece could carry a residual, so no such literal existed.
    ///
    /// Runs only inside `superblock_size_global_suite`.
    fn a_rect128_residual_half_under_a_per_unit_cdef_list_decodes_exact() {
        let _knobs = crate::speed::knob_write();
        if !have_ffmpeg() {
            eprintln!("SKIP: no ffmpeg");
            return;
        }
        force_sb128(true);
        let dir = std::env::temp_dir().join(format!("ec-av1-rectres-cdef-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("testsrc2-1280x768.y4m");
        // Detailed synthetic content: its CDEF search spends more than one
        // strength pair, which is what puts a `cdef_idx` literal in the tile.
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi"])
            .args(["-i", "testsrc2=size=1280x768:rate=24"])
            .args(["-frames:v", "12", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .expect("ffmpeg failed to run")
            .success();
        assert!(ok, "ffmpeg could not write the synthetic clip");
        let (width, height) = (1280usize, 768usize);
        let pictures = crate::probe::source(clip.to_str().unwrap(), "0", "null", width, height, 12);
        force_b128_rect(1);
        force_b128_rectres(true);
        let _ = crate::tile::take_sb128_rectres_hits();
        let _ = crate::filter_search::take_cdef_covered_units();
        let encoded = encode_sequence(&pictures, 90, 0.5).unwrap();
        let coded = crate::tile::take_sb128_rectres_hits();
        let covered = crate::filter_search::take_cdef_covered_units();
        eprintln!(
            "rect halves WITH a residual: writer coded {coded}; 64x64 CDEF units taking \
             another unit's literal: {covered}"
        );
        assert!(coded > 0, "no 128x64 half carried a residual on this clip");
        assert!(
            covered > 0,
            "no 64x64 unit took a non-zero preset from another unit's literal, so this clip \
             cannot witness the rule",
        );
        let decoded = crate::stream::decode_stream(&encoded.stream).expect("our decoder");
        assert_eq!(decoded.len(), encoded.frames.len(), "frame count");
        for (i, (frame, dec)) in encoded.frames.iter().zip(&decoded).enumerate() {
            for (plane, got, want) in [
                ("luma", &dec.y, &frame.reconstruction.y),
                ("U", &dec.u, &frame.reconstruction.u),
                ("V", &dec.v, &frame.reconstruction.v),
            ] {
                let differ = got.iter().zip(want.iter()).filter(|(a, b)| a != b).count();
                assert_eq!(differ, 0, "frame {i}: {plane} -- {differ} samples differ");
                assert_eq!(
                    got.len(),
                    want.len(),
                    "frame {i}: {plane} is {} samples, the reconstruction's is {}",
                    got.len(),
                    want.len()
                );
            }
        }
        let ff = ffmpeg_decode_sequence(&encoded.stream, width, height, pictures.len());
        assert_eq!(
            encoded.frames.len(),
            ff.len(),
            "frame count: ours vs ffmpeg"
        );
        for (i, (frame, dec)) in encoded.frames.iter().zip(&ff).enumerate() {
            assert_eq!(dec.y, frame.reconstruction.y, "frame {i}: ffmpeg luma");
            assert_eq!(dec.u, frame.reconstruction.u, "frame {i}: ffmpeg U");
            assert_eq!(dec.v, frame.reconstruction.v, "frame {i}: ffmpeg V");
        }
    }

    /// lane-b128r: the COMPOUND arm at the 128 root -- one 128x128 block
    /// naming `LAST` plus `GOLDEN`/`ALTREF`, through the same
    /// `write_compound_block` chain every smaller compound block takes, at a
    /// window it had never been exercised at. RED before this lane: the
    /// search passed an empty compound list at the 128 root, so the count was
    /// 0 and no such block existed to decode.
    ///
    /// Runs only inside `superblock_size_global_suite`.
    fn a_128_root_compound_block_decodes_exact_through_both_decoders() {
        let _knobs = crate::speed::knob_write();
        if !have_ffmpeg() {
            eprintln!("SKIP: no ffmpeg");
            return;
        }
        force_sb128(true);
        let dir = std::env::temp_dir().join(format!("ec-av1-b128c-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("testsrc2-1280x768.y4m");
        // Moving objects that occlude each other: a pan is coded by one
        // reference outright, and a compound pair only pays where the two
        // references disagree.
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi"])
            .args(["-i", "testsrc2=size=1280x768:rate=24"])
            .args(["-frames:v", "12", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .expect("ffmpeg failed to run")
            .success();
        assert!(ok, "ffmpeg could not write the synthetic clip");
        let (width, height) = (1280usize, 768usize);
        let pictures = crate::probe::source(clip.to_str().unwrap(), "0", "null", width, height, 12);
        // The compound candidate wins the 128 block's own search on this pan
        // but the root then loses to its four cells, so the root is forced
        // (the residual arm is left at its default, off).
        force_b128_root(true);
        let _ = take_b128_compound_hits();
        let _ = crate::tile::take_sb128_compound_hits();
        let encoded = encode_sequence(&pictures, 150, 0.5).unwrap();
        let (won, coded) = (
            take_b128_compound_hits(),
            crate::tile::take_sb128_compound_hits(),
        );
        eprintln!("COMPOUND 128x128 blocks: search won {won}, writer coded {coded}");
        assert!(
            coded > 0,
            "no compound 128x128 block was coded on this clip"
        );
        let decoded = crate::stream::decode_stream(&encoded.stream).expect("our decoder");
        assert_eq!(decoded.len(), encoded.frames.len(), "frame count");
        for (i, (frame, dec)) in encoded.frames.iter().zip(&decoded).enumerate() {
            for (plane, got, want) in [
                ("luma", &dec.y, &frame.reconstruction.y),
                ("U", &dec.u, &frame.reconstruction.u),
                ("V", &dec.v, &frame.reconstruction.v),
            ] {
                let differ = got.iter().zip(want.iter()).filter(|(a, b)| a != b).count();
                assert_eq!(differ, 0, "frame {i}: {plane} -- {differ} samples differ");
                assert_eq!(
                    got.len(),
                    want.len(),
                    "frame {i}: {plane} is {} samples, the reconstruction's is {}",
                    got.len(),
                    want.len()
                );
            }
        }
        let ff = ffmpeg_decode_sequence(&encoded.stream, width, height, pictures.len());
        assert_eq!(
            encoded.frames.len(),
            ff.len(),
            "frame count: ours vs ffmpeg"
        );
        for (i, (frame, dec)) in encoded.frames.iter().zip(&ff).enumerate() {
            assert_eq!(dec.y, frame.reconstruction.y, "frame {i}: ffmpeg luma");
            assert_eq!(dec.u, frame.reconstruction.u, "frame {i}: ffmpeg U");
            assert_eq!(dec.v, frame.reconstruction.v, "frame {i}: ffmpeg V");
        }
    }

    /// lane-b128hv: the 128 root's `PARTITION_HORZ` and `PARTITION_VERT`
    /// arms -- two 128x64 / 64x128 inter blocks where the NONE arm codes one
    /// square. Three passes over one `testsrc2` clip:
    ///
    ///  * HORZ forced at every root ([`force_b128_rect`]), so every context
    ///    the 128x64 shape reads is exercised rather than the handful the RD
    ///    would pick;
    ///  * VERT forced the same way;
    ///  * UNFORCED, which is the count the gate rows are read against -- the
    ///    same clip with the RD free to choose among NONE / HORZ / VERT /
    ///    four superblocks.
    ///
    /// Each pass is sample-exact against the encoder's own reconstruction
    /// through `decode_stream` AND through ffmpeg; a writer/reader mismatch
    /// surfaces here as a refusal or a pixel miss, never in the BD gate. RED
    /// before this lane by construction: `write_sb128_root` could only name
    /// `PARTITION_NONE` or `PARTITION_SPLIT`, so the count was 0 on all three.
    ///
    /// Runs only inside `superblock_size_global_suite`.
    fn a_128_root_horz_and_vert_halves_decode_exact_through_both_decoders() {
        let _knobs = crate::speed::knob_write();
        if !have_ffmpeg() {
            eprintln!("SKIP: no ffmpeg");
            return;
        }
        force_sb128(true);
        let dir = std::env::temp_dir().join(format!("ec-av1-b128hv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("testsrc2-1280x768.y4m");
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi"])
            .args(["-i", "testsrc2=size=1280x768:rate=24"])
            .args(["-frames:v", "12", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .expect("ffmpeg failed to run")
            .success();
        assert!(ok, "ffmpeg could not write the synthetic clip");
        let (width, height) = (1280usize, 768usize);
        let pictures = crate::probe::source(clip.to_str().unwrap(), "0", "null", width, height, 12);
        let mut unforced = (0usize, 0usize);
        for (label, force) in [("HORZ", 1u8), ("VERT", 2), ("unforced", 0)] {
            force_b128_rect(force);
            let _ = take_b128_horz_hits();
            let _ = take_b128_vert_hits();
            let _ = crate::tile::take_sb128_horz_hits();
            let _ = crate::tile::take_sb128_vert_hits();
            let encoded = encode_sequence(&pictures, 150, 0.5).unwrap();
            let (won_h, won_v) = (take_b128_horz_hits(), take_b128_vert_hits());
            let (coded_h, coded_v) = (
                crate::tile::take_sb128_horz_hits(),
                crate::tile::take_sb128_vert_hits(),
            );
            eprintln!(
                "{label}: search won {won_h} HORZ + {won_v} VERT roots, writer coded \
                 {coded_h} 128x64 + {coded_v} 64x128 halves"
            );
            match force {
                1 => assert!(coded_h > 0, "no 128x64 half was coded with HORZ forced"),
                2 => assert!(coded_v > 0, "no 64x128 half was coded with VERT forced"),
                // The unforced count is REPORTED, not asserted: it is the
                // number the gate is read against, and a zero here would be
                // an RD result, not a defect (the forced passes already prove
                // the writer and the reader agree on the shape).
                _ => unforced = (won_h, won_v),
            }
            let decoded = crate::stream::decode_stream(&encoded.stream).expect("our decoder");
            assert_eq!(decoded.len(), encoded.frames.len(), "{label}: frame count");
            for (i, (frame, dec)) in encoded.frames.iter().zip(&decoded).enumerate() {
                for (plane, got, want) in [
                    ("luma", &dec.y, &frame.reconstruction.y),
                    ("U", &dec.u, &frame.reconstruction.u),
                    ("V", &dec.v, &frame.reconstruction.v),
                ] {
                    let differ = got.iter().zip(want.iter()).filter(|(a, b)| a != b).count();
                    assert_eq!(
                        differ, 0,
                        "{label} frame {i}: {plane} -- {differ} samples differ"
                    );
                    assert_eq!(
                        got.len(),
                        want.len(),
                        "{label} frame {i}: {plane} is {} samples, the reconstruction's is {}",
                        got.len(),
                        want.len()
                    );
                }
            }
            let ff = ffmpeg_decode_sequence(&encoded.stream, width, height, pictures.len());
            assert_eq!(
                encoded.frames.len(),
                ff.len(),
                "{label}: frame count: ours vs ffmpeg"
            );
            for (i, (frame, dec)) in encoded.frames.iter().zip(&ff).enumerate() {
                assert_eq!(
                    dec.y, frame.reconstruction.y,
                    "{label} frame {i}: ffmpeg luma"
                );
                assert_eq!(dec.u, frame.reconstruction.u, "{label} frame {i}: ffmpeg U");
                assert_eq!(dec.v, frame.reconstruction.v, "{label} frame {i}: ffmpeg V");
            }
        }
        force_b128_rect(0);
        eprintln!(
            "unforced on testsrc2: {} HORZ + {} VERT roots over 12 frames",
            unforced.0, unforced.1
        );
    }

/// Restores the process-global 128-superblock state the folded cases move:
/// the sb128 mode back to its prior value, and every `force_b128_*` arm back
/// to off, so one case cannot leak into the next.
fn restore_superblock_globals(prior_sb128: u8) {
    super::split::ENC_SB128.store(prior_sb128, std::sync::atomic::Ordering::Relaxed);
    force_b128_residual(false);
    force_b128_root(false);
    force_b128_rectres(false);
    force_b128_rect(0);
    force_b128_ab(0);
}

/// The ten cases that set the process-global superblock size, folded into
/// one ignored test so they never share a process with another such test.
/// Each case runs under `catch_unwind`; every failing name is reported.
///
///     cargo test -p ec-av1 --release --lib -- --ignored \
///         superblock_size_global_suite --nocapture
#[test]
#[ignore = "sets the process-global superblock size: run it alone"]
fn superblock_size_global_suite() {
    let prior = super::split::ENC_SB128.load(std::sync::atomic::Ordering::Relaxed);
    let cases: [(&str, fn()); 10] = [
        (
            "a_128_superblock_clip_whose_root_search_codes_128x128_blocks_decodes_exact",
            a_128_superblock_clip_whose_root_search_codes_128x128_blocks_decodes_exact,
        ),
        (
            "a_128_root_block_with_a_real_residual_decodes_exact_through_both_decoders",
            a_128_root_block_with_a_real_residual_decodes_exact_through_both_decoders,
        ),
        (
            "a_128_root_residual_block_under_a_per_unit_cdef_list_decodes_exact",
            a_128_root_residual_block_under_a_per_unit_cdef_list_decodes_exact,
        ),
        (
            "a_rect128_half_with_a_real_residual_decodes_exact_through_both_decoders",
            a_rect128_half_with_a_real_residual_decodes_exact_through_both_decoders,
        ),
        (
            "an_ab128_piece_with_a_real_residual_decodes_exact_through_both_decoders",
            an_ab128_piece_with_a_real_residual_decodes_exact_through_both_decoders,
        ),
        (
            "a_rect128_residual_half_under_a_per_unit_cdef_list_decodes_exact",
            a_rect128_residual_half_under_a_per_unit_cdef_list_decodes_exact,
        ),
        (
            "a_128_root_compound_block_decodes_exact_through_both_decoders",
            a_128_root_compound_block_decodes_exact_through_both_decoders,
        ),
        (
            "a_128_root_horz_and_vert_halves_decode_exact_through_both_decoders",
            a_128_root_horz_and_vert_halves_decode_exact_through_both_decoders,
        ),
        (
            "a_128_root_ab_shapes_decode_exact_through_both_decoders",
            super::more_tests::a_128_root_ab_shapes_decode_exact_through_both_decoders,
        ),
        (
            "a_static_clip_codes_128x128_superblock_roots_and_decodes_sample_exact",
            crate::encoder::tests::a_static_clip_codes_128x128_superblock_roots_and_decodes_sample_exact,
        ),
    ];
    let mut failures = Vec::new();
    for (name, case) in cases {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(case)).is_err() {
            failures.push(name);
        }
        restore_superblock_globals(prior);
    }
    assert!(failures.is_empty(), "failed cases: {failures:?}");
}
