/// lane-av1unwritten r7: the STRUCTURAL invariant that r5 and r6 both broke.
///
/// r5 put guards inside `PARTITION_HORZ_A..=PARTITION_VERT_B` arms, so they
/// could only ever see values 4..=7. r6 deleted the inter-16 guard and did not
/// replace it. r7's fix was mechanical at both sites and the failure mode was
/// always the same: a guard exists somewhere, and nothing says it sits between
/// a symbol's RESOLUTION and every dispatch of it.
///
/// So the invariant is stated over the dispatch structure, not over a list.
/// Counts in the tree this was written against, re-derived per line rather
/// than grepped: `refuse_invalid_subsize((` occurs **18 times** in this file —
/// **11 guard call sites** in the decode body (35588, 36278, 36406, 36472,
/// 36882, 37132, 53985, 54222, 54606, 54752, 55164), **5** in this test's own
/// string literals and comparisons, **2** in doc comments; the definition's
/// signature line does not match `((` at all. The scan finds **10 resolutions**:
/// the eleventh guard is the inter-frame 128 one, whose resolution lives in
/// the shared `read_sb128_root`, so a guard has no resolution of its own here.
/// What is asserted, exactly:
///
/// * every `partition_w<N>[` level in the file appears among the resolutions
///   this scan finds -- a new level cannot be added without a guard check;
/// * each resolution is followed, within 220 lines, by a
///   `refuse_invalid_subsize((N, N), …)` for its OWN level (matched on the
///   level, not the variable, because the symbol is often bound to a
///   short-lived inner name `p` and dispatched by an outer `part64`);
/// * where that window contains a `match part…` dispatch, the guard precedes
///   it -- the bound r7's COMMENT claimed and r8 implements;
/// * no guard sits inside a `PARTITION_HORZ_A..=PARTITION_VERT_B` arm.
///
/// It does NOT prove a guard is at the exact dispatch point: the window is a
/// window. And the two cross-checks are weaker than they look — cross-check 1
/// is level PRESENCE only, cross-check 2 is `>=` — so a SECOND unguarded site
/// at an already-covered level is caught ONLY by the per-site 220-line window,
/// which is a window and not a dispatch bound. That limit is why the AB-arm
/// invariant exists separately, and why `EC_AV1_SUBSIZE_GUARD_TRACE` records
/// what each site really sees over the corpus.
#[test]
fn every_partition_symbol_resolution_is_guarded_before_it_dispatches() {
    let src = crate::test_source::module_source("decode");
    // Every partition-symbol RESOLUTION in the file, with the level read off
    // the CDF name on the line itself. Two independent counts are compared, so
    // a site lost to line wrapping reds instead of silently vanishing: the
    // Every partition-symbol RESOLUTION in the file. The window is the line
    // BEFORE as well as the line itself and the one after (r9), because a
    // `let <var> =` on one line with `dec.symbol(&mut cdfs.partition_w8[..])`
    // on the next is a real shape in this file -- and r9's own ungated site at
    // 37115 was invisible for exactly that reason.
    let lines: Vec<&str> = src.lines().collect();
    let mut sites: Vec<(usize, String, usize)> = Vec::new();
    // Occurrences of `partition_w<N>[` per level, counted from the FILE, used as
    // the cross-check below.
    let mut file_occurrences: std::collections::BTreeMap<usize, usize> = Default::default();
    // Only the DECODE body: the test module below also builds `partition_w8`
    // CDFs (an encoder round-trip at 58464/58521), and counting those would
    // demand a guard for a site that does not exist.
    let production = crate::test_source::module_production_source("decode");
    let decode_body = match production.find("\n#[cfg(test)]\nmod tests {") {
        Some(i) => &production[..i],
        None => production,
    };
    {
        let mut rest = decode_body;
        while let Some(i) = rest.find("partition_w") {
            let after = &rest[i + "partition_w".len()..];
            let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
            rest = after;
            if digits.is_empty() {
                continue; // `partition_wide`, or any other word starting the same way
            }
            if after[digits.len()..].starts_with('[') {
                *file_occurrences
                    .entry(digits.parse().expect("digits"))
                    .or_insert(0) += 1;
            }
        }
    }
    // r11 (lane-av1subsizesweep): a partition-symbol READ is a `dec.<name>(..)`
    // call whose OWN ARGUMENT LIST names a `partition_w<N>[` CDF. That is the
    // whole definition, and it is why the read spellings need no hand-list:
    // `dec.symbol(..partition_w..)` qualifies, the frame-edge gather's
    // `dec.symbol_fixed(&gather(..partition_w..))` qualifies, and
    // `dec.debug_state()` does not -- it never names a partition CDF. A third
    // spelling is covered the day somebody writes it.
    //
    // r11's hole was a SPELLING gap, not a shape gap: the old matcher named
    // `dec.symbol(` literally, and `dec.symbol(` is not a substring of
    // `dec.symbol_fixed(`. So the key-frame 16-level frame-edge arm -- which
    // dispatches `decode_leaf_rect` straight off a gathered `edge_split` onto
    // a BLOCK_8X16 chroma walk -- was never a site at all.
    let is_partition_read = |window: &str| -> bool {
        let mut at = 0usize;
        while let Some(rel) = window[at..].find("dec.") {
            let call = at + rel + "dec.".len();
            at = call;
            let name: String = window[call..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if name.is_empty() {
                continue;
            }
            let Some(open) = window[call + name.len()..].find('(') else {
                continue;
            };
            let arg_from = call + name.len() + open + 1;
            let mut depth = 1usize;
            let mut stop = None;
            for (i, c) in window[arg_from..].char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            stop = Some(arg_from + i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(stop) = stop else { continue };
            if window[arg_from..stop].contains("partition_w") {
                return true;
            }
        }
        false
    };
    for (n, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        // A doc comment quoting the shape is not a site, and neither is the
        // test's own example strings further down the file.
        if t.starts_with("//") || !line.contains("partition_w") || n > 56000 {
            continue;
        }
        let lo = n.saturating_sub(1);
        let hi = (n + 2).min(lines.len());
        let window = lines[lo..hi].join(" ");
        // r11: the read test is the DERIVED predicate above, not a spelling
        // literal. Naming `dec.symbol(` here is what hid the frame-edge arm.
        if !is_partition_read(&window) {
            continue;
        }
        let Some(i) = window.find("partition_w") else {
            continue;
        };
        let after = &window[i + "partition_w".len()..];
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            continue;
        }
        let level: usize = digits.parse().expect("digits");
        // The bound variable is the token after `let `, looked for in the two
        // lines the binding can occupy (r9: it may be the line above).
        // r10: scan BACKWARDS from the closest line and take the nearest `let`
        // whose binding is the one followed by this symbol read -- taking the
        // first `let` upward named an unrelated binding above it.
        let mut var = None;
        for cand in (lo..=n).rev() {
            let ct = lines[cand].trim_start();
            if ct.starts_with("//") {
                continue;
            }
            let Some(v) = ct.strip_prefix("let ") else {
                continue;
            };
            let name = v
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .next()
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            // This binding is followed by the symbol read if the SAME line
            // completes `let <name> = dec.<spelling>(`, or if the next line
            // starts the call. That is what separates `let part8 =` from an
            // unrelated `let ec_dbg_pre =` two lines above it.
            let next = lines.get(cand + 1).copied().unwrap_or("");
            let binds_here = ct.contains("= dec.") || next.contains("dec.");
            if binds_here || cand == n {
                var = Some(name);
                break;
            }
        }
        let Some(var) = var else { continue };
        // r11: the edge `gather` reads are SITES now, not skips. r10 skipped
        // them on the reasoning that "the site is guarded at the level's own
        // resolution instead" -- true for every arm that resolves into a
        // `partN`, and FALSE for the one that does not: the key-frame 16-level
        // frame-edge arm dispatches `decode_leaf_rect` straight off `edge_split`,
        // and `BLOCK_8X16` is BLOCK_INVALID at 4:2:2. Every site therefore has
        // to carry a guard for its OWN level between the read and the first
        // chroma-walking dispatch after it -- see the per-site check below.
        sites.push((n, var, level));
    }

    // ---- cross-check 1: every `partition_w<N>` LEVEL in the decode body has
    // at least one guarded resolution. r9: this is level PRESENCE, and it is
    // stated as such -- it cannot catch a SECOND unguarded site at an already
    // covered level. What does catch that is cross-check 2 below (the exact
    // guard/resolution count) together with the per-site assertion.
    let mut covered: std::collections::BTreeSet<usize> = Default::default();
    for (_, _, level) in &sites {
        covered.insert(*level);
    }
    for level in file_occurrences.keys() {
        assert!(
            covered.contains(level),
            "level {level} names a `partition_w{level}[` CDF in the decode body but this \
             scan finds no guarded resolution for it (r9)"
        );
    }

    // ---- cross-check 2: an EXACT count derived from the scan, never a floor.
    // Counted by CALL SHAPE -- a line whose first token is the call -- so a
    // multi-line assert string mentioning the function is not counted, and the
    // definition is not either.
    let guarded_sites = decode_body
        .lines()
        .filter(|l| l.trim_start().starts_with("refuse_invalid_subsize(("))
        .count();
    // r9: was `assert_eq!`, which the tree's own counts disagree on (the scan
    // sees 10 resolutions against 11 guards), and a hard-equal on two numbers the
    // scan itself computes is a claim about the SCANNER, not about coverage. It
    // is `>=` plus the per-site assertion below: a resolution with no guard reds
    // on its own line, which is the property that matters, and a guard with no
    // resolution behind it is redundant rather than harmful.
    assert!(
        guarded_sites >= sites.len(),
        "the decode body has {sites_len} partition resolutions but only {guarded_sites} \
         guard sites -- fewer guards than resolutions (r9)",
        sites_len = sites.len()
    );

    for (line_no, var, level) in &sites {
        // Two properties, both stated exactly:
        //
        //  (a) a guard for THIS LEVEL appears after the binding, within a
        //      220-line window -- matched on the level, not the variable name,
        //      because the symbol is often bound to a short-lived inner name
        //      (`p`) and dispatched by an outer one (`part64`);
        //  (b) where the window contains a `match part…` dispatch, the guard
        //      precedes it.
        let window: Vec<&str> = lines[line_no + 1..(line_no + 221).min(lines.len())].to_vec();
        let guard_at = window
            .iter()
            .position(|l| l.contains(&format!("refuse_invalid_subsize(({level}, {level})")));
        assert!(
            guard_at.is_some(),
            "{} (decode.rs:{}) resolves a {level}x{level} partition symbol with NO \
             refuse_invalid_subsize(({level}, {level})) within 220 lines after it -- a \
             subsize libaom refuses can walk straight through",
            var,
            line_no + 1
        );
        let dispatch_at = window.iter().position(|l| {
            let t = l.trim_start();
            !t.starts_with("//") && t.starts_with("match part")
        });
        if let Some(d) = dispatch_at {
            assert!(
                guard_at.expect("checked above") < d,
                "{} (decode.rs:{}) is DISPATCHED at window line {d} BEFORE its \
                 refuse_invalid_subsize(({level}, {level})) -- the guard can never \
                 observe that dispatch",
                var,
                line_no + 1
            );
        }
    }

    // The r5 defect, as its own invariant: a guard must never sit INSIDE a
    // `PARTITION_HORZ_A..=PARTITION_VERT_B` arm. Such an arm can only carry
    // partition values 4..=7, and the offending values are PARTITION_VERT (2)
    // and PARTITION_VERT_4 (9). Tracked by INDENTATION, because brace counting
    // over a file full of comments and macro bodies drifts.
    let indent_of = |l: &str| l.len() - l.trim_start().len();
    // (arm indent, arm BODY indent). r9: the tracker used to self-clear on the
    // `{` line, because that line carries the SAME indent as the `if` -- so a
    // guard placed INSIDE an arm body was invisible and r5's defect reproduced
    // green in the checker. Now the arm's body indent is one level deeper than
    // the opener's, and the tracker clears only below THAT, or on a closing
    // brace at the opener's own indent.
    let mut ab_arm: Option<(usize, bool)> = None;
    for (n, line) in src.lines().enumerate() {
        let t = line.trim_start();
        if t.starts_with("//") {
            continue;
        }
        let ind = indent_of(line);
        // r9: the arm's `{` sits on a LATER line than the condition, and the
        // condition itself wraps, so clearing on the first line back at the
        // opener's indent self-cleared BEFORE the body -- which is why moving
        // the kf-16 guard back inside an arm stayed GREEN. Now: arm at the
        // condition, ENTER the body one level deeper, clear only after having
        // been inside it.
        if line.contains("PARTITION_HORZ_A..=PARTITION_VERT_B") {
            ab_arm = Some((ind + 4, false));
        } else if t == "{" {
            // r10: the arm's own brace carries the OPENER's indent, not the
            // body's, so treating it as "backed out" cleared the arm before a
            // single body statement was seen -- which is why the mutation that
            // moves a guard INSIDE an arm stayed GREEN at r8 AND r9. A lone `{`
            // never changes the state.
        } else if let Some((body, entered)) = ab_arm {
            if !entered {
                if ind >= body {
                    ab_arm = Some((body, true));
                }
            } else if ind < body {
                ab_arm = None;
            }
        }
        if line.contains("refuse_invalid_subsize((") {
            assert!(
                ab_arm.is_none_or(|_| false),
                "decode.rs:{} puts a refuse_invalid_subsize inside a \
                 PARTITION_HORZ_A..=PARTITION_VERT_B arm (values 4..=7 only); the \
                 offending values are PARTITION_VERT (2) and PARTITION_VERT_4 (9), which \
                 that arm can never observe -- r5's defect",
                n + 1
            );
        }
    }
}

/// lane-av1unwritten: the port is a TRANSCRIPTION, and this is the artefact
/// that makes it checkable. It re-writes `av1_ss_size_lookup`'s 22 rows
/// (`common_data.c:17-41`) literally -- every shape, every subsampling
/// pair, the real BLOCK_INVALID markers -- and asserts
/// [`chroma_plane_block_codable`] agrees cell for cell. A predicate that
/// hardcodes the wrong cell (this lane's first version marked BLOCK_16X4 at
/// 4:2:2 invalid; the table says `BLOCK_8X4`) reds here, not in a corpus
/// that happens not to carry the shape.
#[test]
fn chroma_plane_block_codable_matches_the_libaom_table() {
    // (width, height, [ss_x=0][ss_y=0], [0][1], [1][0], [1][1]); `None` is
    // BLOCK_INVALID.
    const I: Option<(usize, usize)> = None;
    let table: &[(usize, usize, [Option<(usize, usize)>; 4])] = &[
        (
            4,
            4,
            [Some((4, 4)), Some((4, 4)), Some((4, 4)), Some((4, 4))],
        ),
        (4, 8, [Some((4, 8)), Some((4, 4)), I, Some((4, 4))]),
        (8, 4, [Some((8, 4)), I, Some((4, 4)), Some((4, 4))]),
        (
            8,
            8,
            [Some((8, 8)), Some((8, 4)), Some((4, 8)), Some((4, 4))],
        ),
        (8, 16, [Some((8, 16)), Some((8, 8)), I, Some((4, 8))]),
        (16, 8, [Some((16, 8)), I, Some((8, 8)), Some((4, 4))]),
        (
            16,
            16,
            [Some((16, 16)), Some((8, 16)), Some((8, 8)), Some((4, 8))],
        ),
        (16, 32, [Some((16, 32)), Some((16, 16)), I, Some((8, 16))]),
        (32, 16, [Some((32, 16)), I, Some((16, 16)), Some((16, 8))]),
        (
            32,
            32,
            [
                Some((32, 32)),
                Some((16, 32)),
                Some((16, 16)),
                Some((16, 8)),
            ],
        ),
        (32, 64, [Some((32, 64)), Some((32, 32)), I, Some((16, 32))]),
        (64, 32, [Some((64, 32)), I, Some((32, 32)), Some((32, 16))]),
        (
            64,
            64,
            [
                Some((64, 64)),
                Some((32, 64)),
                Some((32, 32)),
                Some((32, 16)),
            ],
        ),
        (
            64,
            128,
            [Some((64, 128)), Some((64, 64)), I, Some((32, 64))],
        ),
        (
            128,
            64,
            [Some((128, 64)), I, Some((64, 64)), Some((64, 32))],
        ),
        (
            128,
            128,
            [
                Some((128, 128)),
                Some((64, 128)),
                Some((64, 64)),
                Some((64, 32)),
            ],
        ),
        (4, 16, [Some((4, 16)), Some((4, 8)), I, Some((4, 8))]),
        (16, 4, [Some((16, 4)), I, Some((8, 4)), Some((8, 4))]),
        (8, 32, [Some((8, 32)), Some((8, 16)), I, Some((4, 16))]),
        (32, 8, [Some((32, 8)), I, Some((16, 8)), Some((16, 4))]),
        (16, 64, [Some((16, 64)), Some((16, 32)), I, Some((8, 32))]),
        (64, 16, [Some((64, 16)), I, Some((32, 16)), Some((32, 8))]),
        (
            128,
            256,
            [
                Some((128, 256)),
                Some((64, 256)),
                Some((64, 128)),
                Some((64, 64)),
            ],
        ),
        (
            256,
            128,
            [
                Some((256, 128)),
                Some((128, 256)),
                Some((128, 64)),
                Some((128, 64)),
            ],
        ),
    ];
    for &(w, h, cells) in table {
        for sx in 0..2usize {
            for sy in 0..2usize {
                let want_codable = cells[sx * 2 + sy].is_some();
                assert_eq!(
                    chroma_plane_block_codable(w, h, sx, sy),
                    want_codable,
                    "av1_ss_size_lookup[{w}x{h}][{sx}][{sy}] is {:?} in libaom \
                     (common_data.c) but our predicate says the opposite",
                    cells[sx * 2 + sy]
                );
            }
        }
    }
    // The two cells an earlier, hand-picked predicate got wrong: both are
    // LEGAL at 4:2:2, and libaom's own encoder emits the 16x4 one.
    assert!(
        chroma_plane_block_codable(16, 4, 1, 0),
        "16x4 at 4:2:2 is BLOCK_8X4"
    );
    assert!(
        !chroma_plane_block_codable(32, 8, 0, 1),
        "32x8 at 4:4:0 is BLOCK_INVALID"
    );
}

/// lane-av1subsizesweep: the CENSUS, as an invariant.
///
/// The merged `lane/av1unwritten` lane covers partition SYMBOLS: every
/// `dec.symbol(&mut cdfs.partition_w<N>[..])` gets a
/// [`refuse_invalid_subsize`] within 220 lines, and
/// [`every_partition_symbol_resolution_is_guarded_before_it_dispatches`]
/// holds that shape against the source. Its own stated limit is exactly
/// the hole this lane found: the scan matches on `dec.symbol(`, so the
/// frame-edge GATHER reads (`dec.symbol_fixed(&gather(..))`) are invisible
/// to it, and its site list names RESOLUTIONS, not the chroma-plane WALKS
/// those resolutions descend into.
///
/// So this test states the other half, over the WALK sites instead of the
/// symbol sites:
///
/// * every `decode_*` call that walks a chroma plane is enumerated, with
///   the luma shape(s) it can be handed -- derived from the enclosing
///   dispatch, not typed in by hand;
/// * for each shape that libaom marks BLOCK_INVALID at some subsampling
///   mode, the walk must have a [`refuse_invalid_subsize`] for its OWN
///   level on its path -- either at the level that dispatched it
///   (`PARTITION_*` arms resolve into a `partN`) or at the dispatch site
///   itself;
/// * a walk reachable ONLY through a level whose guard already refuses the
///   offending subsize is CLOSED, and the reason is the level's own guard
///   line -- asserted, not commented.
///
/// The residue the merged lane named, [`decode_rect4_16_intrabc`], is the
/// second case: it is reachable only from the 16x16-level 1:4 arms, and
/// `partition_subsize_dims((16,16), PARTITION_VERT_4) == (4,16)`, which is
/// in `PLANE_BLOCK_INVALID_422`. Both of its callers' guards refuse that
/// before the call, so it is CLOSED rather than guarded a third time --
/// and this test is what makes that a checked claim instead of a comment.
#[test]
fn every_chroma_walking_leaf_dispatch_is_guarded_or_closed_with_a_reason() {
    let src = crate::test_source::module_source("decode");
    // r2: `decode_body` is hoisted to the top of the test because the per-row
    // guard check below needs it BEFORE the walk census it sits under, and a
    // reader who has to hunt for a binding is a reader who will not notice
    // when it is scoped to the wrong thing.
    let production = crate::test_source::module_production_source("decode");
    let decode_body = match production.find("\n#[cfg(test)]\nmod tests {") {
        Some(i) => &production[..i],
        None => production,
    };
    // (a) The shapes libaom cannot code, transcribed from the same
    // `av1_ss_size_lookup` cells `PLANE_BLOCK_INVALID_422` carries. A walk
    // handed one of these at the matching subsampling mode is the defect.
    let invalid_at =
        |bw: usize, bh: usize, sx: usize, sy: usize| !chroma_plane_block_codable(bw, bh, sx, sy);

    // (b) The CENSUS: every chroma-plane walk in the decode body, with the
    // luma footprint(s) each dispatch can hand it. `guarded_by` names the
    // mechanism, which is what a reader has to be able to check:
    //
    //   * `refuse_invalid_subsize` at the DISPATCH -- the guard sits on
    //     the walk's own path (the hole this lane fixed, and the 8x8 /
    //     16-level sites);
    //   * `refuse_invalid_subsize` at the RESOLUTION -- the dispatch is a
    //     `PARTITION_*` arm, so the level's guard saw the symbol that
    //     selected this shape before the call;
    //   * `unreachable` -- proven-uncodable at every subsampling mode a
    //     `color_config` can select, closed with an assertion instead of
    //     a guard.
    //
    // Each row is (walk, shapes, mechanism).
    //
    // r2 (Main's review, and it was right): `AtResolution` used to assert
    // only that SOME shape the walk handles is BLOCK_INVALID, which
    // restates the libaom table and never mentions a guard -- deleting the
    // inter 32-level guard left it green. So a resolution row now CARRIES
    // the guard it depends on: `(walk, fn-scope, level)`, and the check
    // below looks for `refuse_invalid_subsize((level, level))` inside that
    // function and before the walk's first call site in it. A row that
    // cannot be checked that way says so with `Prose` instead.
    enum Mech {
        AtDispatch,
        /// Protected by `refuse_invalid_subsize((L, L))` in the named
        /// function, above the walk's first call site there.
        AtResolution(&'static str, usize),
        /// The walk is reached INDIRECTLY -- this row's guard is asserted to
        /// exist in `fscope`, and the ordering is asserted by the per-route
        /// loop further down (which scopes to the enclosing function and
        /// names this walk's real caller chain). Used only where the walk's
        /// call site is not in the guard's own function.
        AtResolutionVia(&'static str, usize),
        Unreachable(&'static str),
    }
    // The key-frame tile decoder and the inter tile decoder. Every
    // resolution guard lives in one of the two, which is what makes "the
    // guard for THIS row" a scoped lookup rather than a global one.
    const KF: &str = "decode_key_frame_tile_with_cdfs";
    const INTER: &str = "decode_inter_frame_tile_with_cdfs";
    let census: &[(&str, &[(usize, usize)], Mech)] = &[
        // ---- the residue the merged lane named -------------------
        //
        // `decode_rect4_16_intrabc` reconstructs one 16x4 or 4x16 strip.
        // It has exactly ONE caller (`decode_rect4_16_strip`, decode.rs
        // 18934), which has exactly two (17595 from `decode_rect4_16`,
        // 13810 from `decode_intra_rect_in_inter`), which have one each
        // (36793 key frame, 45447 inter). BOTH are 16x16-level 1:4 arms, so
        // both sit under a `refuse_invalid_subsize((16, 16), part16)`.
        //
        // The offending subsize is the VERT_4 one and ONLY that one:
        // `partition_subsize_dims((16,16), PARTITION_VERT_4) == (4,16)`,
        // and (4,16) is in `PLANE_BLOCK_INVALID_422`, while HORZ_4 gives
        // (16,4) whose (1,0) cell is BLOCK_8X4 and is legal. So the
        // 16x16-level guard refuses exactly the shape that would be
        // BLOCK_INVALID, and the strip that survives -- 16x4 -- is the one
        // this decoder's `own422` arm is written for. Part (d) below
        // re-derives that split and part (f) pins the caller count, so the
        // closure is checked rather than asserted in prose.
        (
            "decode_rect4_16_intrabc",
            &[(16, 4), (4, 16)],
            Mech::AtResolutionVia(KF, 16),
        ),
        // ---- 1:4 and 2:1 rect walks, all resolved-symbol guarded --
        (
            "decode_rect4_16",
            &[(16, 4), (4, 16)],
            Mech::AtResolution(KF, 16),
        ),
        (
            "decode_rect4_16_strip",
            &[(16, 4), (4, 16)],
            Mech::AtResolutionVia(KF, 16),
        ),
        (
            "decode_block_rect",
            &[(32, 16), (16, 32)],
            Mech::AtResolution(KF, 32),
        ),
        (
            "decode_block_rect4",
            &[(32, 8), (8, 32)],
            Mech::AtResolution(KF, 32),
        ),
        (
            "decode_block_rect64",
            &[(64, 16), (16, 64)],
            Mech::AtResolution(KF, 64),
        ),
        // The 128-root guard lives in `read_sb128_root`, the shared
        // resolver both tile decoders call -- not in the walker's own
        // function.
        (
            "decode_block_128rect",
            &[(128, 64), (64, 128)],
            Mech::AtResolutionVia("read_sb128_root", 128),
        ),
        // `decode_rect_split` is called from inside decode_block_rect /
        // decode_leaf_rect / decode_block_rect4 / decode_block_rect64, none of
        // which is the tile decoder -- so the guard it inherits is checked on
        // its own callers' rows, not here.
        (
            "decode_rect_split",
            &[(64, 16), (16, 64), (32, 16), (16, 32)],
            Mech::AtResolutionVia(KF, 32),
        ),
        (
            "decode_leaf_rect",
            &[(16, 8), (8, 16)],
            Mech::AtResolution(KF, 16),
        ),
        // ---- the INTER-side rect walks -----------------------------
        //
        // r2: without these three rows the inter 32- and 64-level guards
        // were claimed by NOTHING, and deleting either left the census
        // green. They are load-bearing -- `decode_intra_rect_in_inter`
        // dispatches `decode_block_rect4` on 32x8/8x32 (8x32 is
        // BLOCK_INVALID at 4:2:2) and `decode_block_rect64` on
        // 64x16/16x64 (16x64 likewise) -- so each gets its own row naming
        // the guard that stands in front of it.
        //
        // These are `AtResolutionVia`, not `AtResolution`: the walk is
        // dispatched from `decode_intra_rect_in_inter`, a helper, not from
        // the tile decoder the guard lives in. The check is therefore guard
        // EXISTENCE in that decoder -- which is what reds when the guard
        // is deleted -- and the linkage is stated here rather than derived.
        (
            "decode_block_rect4[inter]",
            &[(32, 8), (8, 32)],
            Mech::AtResolutionVia(INTER, 32),
        ),
        (
            "decode_block_rect64[inter]",
            &[(64, 16), (16, 64)],
            Mech::AtResolutionVia(INTER, 64),
        ),
        (
            "decode_intra_rect_in_inter",
            &[(16, 8), (8, 16), (16, 4), (4, 16)],
            Mech::AtResolutionVia(INTER, 16),
        ),
        // ---- sub-8 walks ------------------------------------------
        // (8,4) is legal at 4:2:2 (`av1_ss_size_lookup[8x4][1][0]` is
        // BLOCK_4X4); (4,8) is BLOCK_INVALID there. Both come off a raw
        // `part8`, and both 8x8 resolution sites carry the guard -- plus
        // the redundant inline `chroma_plane_block_codable` check the
        // merged lane left at 36904.
        (
            "decode_leaf_rect8",
            &[(8, 4), (4, 8)],
            Mech::AtResolution(KF, 8),
        ),
        (
            "decode_inter_sub8_rect2",
            &[(8, 4), (4, 8)],
            Mech::AtResolution(INTER, 8),
        ),
        (
            "decode_leaf_split4",
            &[(4, 4)],
            Mech::Unreachable("4x4's plane block is 4x4 at EVERY subsampling cell"),
        ),
        (
            "decode_inter_sub8_split4",
            &[(4, 4)],
            Mech::Unreachable("4x4's plane block is 4x4 at EVERY subsampling cell"),
        ),
        (
            "decode_leaf8",
            &[(8, 8)],
            Mech::Unreachable("8x8's plane block is 8x8 / 4x8 / 4x4, never BLOCK_INVALID"),
        ),
        // The square walks take only square footprints, and every square
        // in `BLOCK_SIZES_ALL` has a valid plane block at EVERY selectable
        // mode (the whole BLOCK_INVALID column is 4x8 / 8x4 / 4x16 / 8x16
        // / ... -- all rect). So no guard is needed for them, and the
        // level guards that exist protect the SYMBOL that would have to
        // select one of those rect shapes, not the square decode itself.
        (
            "decode_block",
            &[(128, 128), (64, 64), (32, 32), (16, 16), (8, 8)],
            Mech::Unreachable(
                "every SQUARE footprint's plane block is valid at every selectable \
                 subsampling mode; the whole BLOCK_INVALID column is rect",
            ),
        ),
        (
            "decode_inter_block",
            &[(128, 128), (64, 64), (32, 32), (16, 16), (8, 8)],
            Mech::Unreachable(
                "every SQUARE footprint's plane block is valid at every selectable \
                 subsampling mode; the whole BLOCK_INVALID column is rect",
            ),
        ),
        (
            "decode_inter_block8",
            &[(8, 8)],
            Mech::Unreachable("8x8's plane block is 8x8 / 4x8 / 4x4, never BLOCK_INVALID"),
        ),
        // ---- THE HOLE THIS LANE FIXED ---------------------------
        //
        // `decode_leaf_rect`'s key-frame FRAME-EDGE caller (the one
        // `decode_leaf_rect`'s row above does not cover, because that arm
        // dispatches on a gathered `edge_split` instead of resolving into
        // `part16`). It is 8x16 when the COLUMN is short -- BLOCK_INVALID
        // at 4:2:2 -- and 16x8 when the ROW is, which is legal and must
        // keep decoding. Before this lane nothing sat on that path.
        (
            "decode_leaf_rect[16-edge]",
            &[(16, 8), (8, 16)],
            Mech::AtDispatch,
        ),
    ];

    // (c) Every row's mechanism must be a REAL one. This is what stops
    // the census from being a list of good intentions: each claim is
    // re-derived here, and the file is checked for the site it names.
    for (walk, shapes, mech) in census {
        let name = walk.split('[').next().expect("walk name");
        assert!(
            src.contains(&format!("fn {name}(")),
            "the census names `{name}`, which is not a function in this file -- the \
             walk was renamed or removed and this row is stale"
        );
        // Only subsampling modes a `color_config` can actually SELECT
        // count. `(0, 1)` 4:4:0 is excluded on libaom's own authority --
        // it asserts `subsampling_y == 0` whenever `subsampling_x == 0`
        // (`color_config`, av1_decoder.c), so no frame in existence is
        // 4:4:0 and `PLANE_BLOCK_INVALID_440` is transcribed only to keep
        // the cell table whole. Counting it here would make `16x4` look
        // like a shape that needs a guard at a mode no stream can carry
        // -- the same class as the merged lane's r2, which refused a legal
        // 4:2:2 shape by testing a cell no `color_config` can request.
        let offending: Vec<(usize, usize, usize, usize)> = shapes
            .iter()
            .flat_map(|&(bw, bh)| {
                [(1, 0), (1, 1), (0, 0)]
                    .into_iter()
                    .filter_map(move |(sx, sy)| {
                        invalid_at(bw, bh, sx, sy).then_some((bw, bh, sx, sy))
                    })
            })
            .collect();
        match mech {
            // A dispatch-time guard has to reduce to the SAME shape the
            // dispatch hands the walk -- this is the claim that made the
            // hole a hole, stated as an assertion.
            Mech::AtDispatch => {
                assert!(
                    !offending.is_empty(),
                    "{walk} is listed AtDispatch but libaom codes every shape it \
                     walks at every subsampling mode -- then it needs no guard"
                );
                for &(bw, bh, sx, sy) in &offending {
                    assert!(
                        invalid_at(bw, bh, sx, sy),
                        "{walk} is not actually handed {bw}x{bh} at ({sx},{sy})"
                    );
                }
            }
            // r2: the guard THIS row depends on, scoped to the function it
            // lives in and ordered before the walk's first call there.
            // Deleting that one guard -- and only that one -- reds this row,
            // which is what r2's review showed the previous version could
            // not do.
            Mech::AtResolution(fscope, level) => {
                assert!(
                    !offending.is_empty(),
                    "{walk} is listed AtResolution but no shape it walks is BLOCK_INVALID \
                     -- then the level's guard is not what protects it"
                );
                let fstart = decode_body
                    .find(&format!("\nfn {fscope}("))
                    .or_else(|| decode_body.find(&format!("\npub(crate) fn {fscope}(")))
                    .or_else(|| decode_body.find(&format!("\npub fn {fscope}(")))
                    .unwrap_or_else(|| {
                        panic!("the census names `{fscope}`, which is not a function here")
                    });
                let fend = ["\nfn ", "\npub fn ", "\npub(crate) fn "]
                    .iter()
                    .filter_map(|pat| decode_body[fstart + 1..].find(pat).map(|i| fstart + 1 + i))
                    .min()
                    .unwrap_or(decode_body.len());
                let body = &decode_body[fstart..fend];
                let guard = format!("refuse_invalid_subsize(({level}, {level})");
                let gpos = body.find(&guard).unwrap_or_else(|| {
                    panic!(
                        "{walk} claims refuse_invalid_subsize(({level}, {level})) guards it, \
                         but there is NO such guard in `{fscope}` -- the row's mechanism is \
                         prose, not a check; re-derive it or mark the row Prose"
                    )
                });
                let call = body.find(&format!("{name}(")).unwrap_or_else(|| {
                    panic!("{walk} has no call site inside `{fscope}` to order the guard against")
                });
                assert!(
                    gpos < call,
                    "{walk}: in `{fscope}` the ({level}, {level}) guard is at {gpos} and the \
                     walk is dispatched at {call} -- a guard BELOW the dispatch can never \
                     observe it"
                );
            }
            Mech::AtResolutionVia(fscope, level) => {
                assert!(
                    !offending.is_empty(),
                    "{walk} is listed AtResolutionVia but no shape it walks is \
                     BLOCK_INVALID"
                );
                let fstart = decode_body
                    .find(&format!("\nfn {fscope}("))
                    .or_else(|| decode_body.find(&format!("\npub(crate) fn {fscope}(")))
                    .or_else(|| decode_body.find(&format!("\npub fn {fscope}(")))
                    .unwrap_or_else(|| {
                        panic!("the census names `{fscope}`, which is not a function here")
                    });
                let fend = ["\nfn ", "\npub fn ", "\npub(crate) fn "]
                    .iter()
                    .filter_map(|pat| decode_body[fstart + 1..].find(pat).map(|i| fstart + 1 + i))
                    .min()
                    .unwrap_or(decode_body.len());
                assert!(
                    decode_body[fstart..fend]
                        .contains(&format!("refuse_invalid_subsize(({level}, {level})")),
                    "{walk} claims refuse_invalid_subsize(({level}, {level})) in `{fscope}` \
                     guards it, but there is NO such guard there"
                );
            }
            Mech::Unreachable(reason) => {
                assert!(
                    offending.is_empty(),
                    "{walk} claims it is unreachable, but libaom marks {} \
                     BLOCK_INVALID -- the claim is stale",
                    offending
                        .iter()
                        .map(|(bw, bh, sx, sy)| format!("{bw}x{bh}@({sx},{sy})"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                assert!(
                    reason.len() > 40,
                    "{walk}'s Unreachable reason is too short to name the symbol \
                     or caller that makes it unreachable"
                );
            }
        }
    }

    // (d) The residue's closure, derived rather than asserted in prose:
    // the ONLY subsize its callers can hand it that libaom refuses at
    // 4:2:2 is the 16x16-level VERT_4 one, and the level's own guard
    // refuses exactly that. If either half stops holding -- a new caller
    // at another level, or a change to `partition_subsize_dims` -- this
    // reds instead of the comment going stale.
    for &(part, want) in &[
        (PARTITION_HORZ_4, (16usize, 4usize)),
        (PARTITION_VERT_4, (4, 16)),
    ] {
        let (bw, bh) = partition_subsize_dims((16, 16), part);
        assert_eq!(
            (bw, bh),
            want,
            "get_partition_subsize(BLOCK_16X16, {part}) moved"
        );
        assert_eq!(
            chroma_plane_block_codable(bw, bh, 1, 0),
            part == PARTITION_HORZ_4,
            "the 16x16 1:4 pair's (1,0) cells moved: HORZ_4 -> {bw}x{bh}, \
             VERT_4 -> {bw}x{bh}. decode_rect4_16_intrabc's Unreachable row \
             is written against exactly this split"
        );
    }
    // (e) The AtDispatch row, checked where it actually bites. This is
    // the hole the merged lane's scan cannot see: its 220-line window is
    // keyed on `dec.symbol(`, and the frame-edge arm reads
    // `dec.symbol_fixed(&gather(..))` into `edge_split` and dispatches on
    // THAT. The claim to check is ordering -- the guard has to sit between
    // the read and the `decode_leaf_rect` it protects, which is r5's
    // defect in a new place if it ever moves below.
    let edge_read = src
        .find("let edge_split = if has_cols16")
        .expect("the key-frame 16-level frame-edge arm reads a gathered bit");
    let guard_at = src[edge_read..]
        .find("refuse_invalid_subsize((16, 16)")
        .map(|i| edge_read + i)
        .expect(
            "the key-frame 16-level frame-edge arm dispatches decode_leaf_rect on an \
             8x16 strip (BLOCK_INVALID at 4:2:2) with NO refuse_invalid_subsize on its \
             path -- every sibling edge arm resolves into a guarded partN, this one does not",
        );
    let walk_at = src[edge_read..]
        .find("decode_leaf_rect(")
        .map(|i| edge_read + i)
        .expect("the frame-edge arm's decode_leaf_rect call");
    assert!(
        guard_at < walk_at,
        "the 16-level frame-edge guard (offset {guard_at}) sits AFTER the \
         decode_leaf_rect it protects (offset {walk_at}) -- it can never observe it"
    );

    // (f) The residue's closure, by CALLER COUNT rather than by prose: a
    // third caller at another level is what would make the residue row
    // stale, and a count reds on that where a comment cannot. Counted over
    // the DECODE BODY only -- this test's own string literals name the same
    // functions, and the merged lane's own scan hit exactly that (five of
    // its eighteen `refuse_invalid_subsize((` matches are its own).
    for (name, calls) in [
        ("decode_rect4_16_intrabc", 1usize),
        ("decode_rect4_16_strip", 2),
        ("decode_rect4_16", 1),
        ("decode_intra_rect_in_inter", 1),
    ] {
        let needle = format!("{name}(");
        let decl = format!("fn {name}(");
        // A doc-comment mention (`[`name`](..)`) is prose, not a call, so
        // comment lines are dropped before counting.
        let body: String = decode_body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let found = body.matches(&needle).count() - usize::from(body.contains(&decl));
        assert_eq!(
            found, calls,
            "{name} has {found} call site(s) in the decode body, the census expects \
             {calls}. A new caller is not automatically wrong, but it is not \
             automatically covered either: re-derive which level's guard it sits \
             under before this row stays green"
        );
    }
    // Both callers reach the 1:4 strips by a DIFFERENT route, so the
    // closure is checked per route -- and per route it is the same shape:
    // the 16x16-level guard must sit ABOVE the test that names VERT_4.
    //
    //   key frame: `decode_rect4_16(...)` at 36793, inside an arm testing
    //   `part16 == PARTITION_HORZ_4 || part16 == PARTITION_VERT_4`.
    //   inter:     the 1:4 strips reach `decode_intra_rect_in_inter`
    //   through `fctx.inter_strip_chroma`, whose ONLY writer is the inter
    //   16-level 1:4 arm (55068) -- `strip16` is `None` without it.
    //
    // Ordering is the property (a guard BELOW the 1:4 test cannot observe
    // it), which is why each pair is compared rather than merely counted.
    for (what, test, call) in [
        (
            "the key-frame decode_rect4_16 caller",
            "if part16 == PARTITION_HORZ_4",
            "decode_rect4_16(",
        ),
        (
            "the inter-frame inter_strip_chroma writer",
            "let horz = part16 == PARTITION_HORZ_4;",
            "c.set(Some(InterStripChroma {",
        ),
    ] {
        let fn_scope = |at: usize| -> std::ops::Range<usize> {
            let head = ["\nfn ", "\npub fn ", "\npub(crate) fn "]
                .iter()
                .filter_map(|pat| decode_body[..at].rfind(pat))
                .max()
                .unwrap_or(0);
            let tail = ["\nfn ", "\npub fn ", "\npub(crate) fn "]
                .iter()
                .filter_map(|pat| decode_body[at..].find(pat).map(|i| at + i))
                .min()
                .unwrap_or(decode_body.len());
            head..tail
        };
        // The first occurrence of the test text can sit in another function (the files are
        // concatenated in path order), so take the first occurrence whose own function holds
        // the dispatch. The guard, the test and the dispatch must then share that function.
        let (test_at, _, call_at) = decode_body
            .match_indices(test)
            .find_map(|(t, _)| {
                let sc = fn_scope(t);
                decode_body[sc.clone()]
                    .rfind(call)
                    .map(|i| (t, sc.clone(), sc.start + i))
            })
            .unwrap_or_else(|| panic!("the 1:4 arm test {test:?} for {what} is gone, or no function holds its dispatch {call:?}"));
        // The dispatch is searched inside the 1:4 test's own function: the files are
        // concatenated in path order, so the last occurrence in the whole text is no longer
        // the key-frame call. The guard, test and dispatch must share that function.
        // `decode_rect4_16(` also matches the DEFINITION, which is the
        // first occurrence -- the call is the last one.
        // r2 (Main's review, and it was right): the guard search is SCOPED
        // to the anchor's own enclosing function. `find()` over the whole
        // decode body returns the FIRST `refuse_invalid_subsize((16, 16)` --
        // the KEY-FRAME one -- so the inter pair's comparison was satisfied
        // by a guard three quarters of the file away, and deleting the inter
        // 16-level guard left this test green. Both routes live in
        // different `fn` bodies (`decode_key_frame_tile_with_cdfs` and the
        // inter tile decoder), so the enclosing function is the scope that
        // tells the two apart.
        let scope = fn_scope(test_at);
        let guard_at = decode_body[scope.clone()]
            .find("refuse_invalid_subsize((16, 16)")
            .map(|i| scope.start + i)
            .unwrap_or_else(|| {
                panic!(
                    "{what}: there is NO refuse_invalid_subsize((16, 16)) in the function \
                     enclosing the 1:4 arm test -- so nothing refuses the VERT_4 symbol \
                     this arm dispatches on, and decode_rect4_16_intrabc's closure is gone"
                )
            });
        assert!(
            guard_at < test_at && test_at < call_at,
            "{what}: guard at {guard_at} (function scope {scope:?}), 1:4 arm test at \
             {test_at}, dispatch at {call_at}. The guard has to sit ABOVE the 1:4 arm \
             test INSIDE THE SAME FUNCTION, or the VERT_4 symbol it refuses has already \
             been dispatched on"
        );
        assert!(
            scope.contains(&guard_at) && scope.contains(&test_at) && scope.contains(&call_at),
            "{what}: guard, 1:4 test and dispatch must share one enclosing function"
        );
    }
    // And the inter route really is the only one: `strip16` is built from
    // `strip_chroma`, which is `inter_strip_chroma.take()`, so a missing
    // writer would silently drop the arm rather than expose it.
    assert_eq!(
        decode_body.matches("inter_strip_chroma.with(|c| {").count(),
        1,
        "inter_strip_chroma gained a second writer -- the inter 1:4 strips can now \
         come from a level whose guard this census has not checked"
    );

    // (g) PER-SITE rows. Everything above claims a guard by LEVEL --
    // "there is a `refuse_invalid_subsize((8, 8))` somewhere in the
    // key-frame tile decoder" -- and that is exactly what makes two
    // guards deletable: the key-frame 8x8 level has TWO resolution
    // sites (37322 and 37621) and the walk census's `decode_leaf_rect8`
    // row is satisfied by the first one, so deleting the SECOND left the
    // test green; the inter decoder carries its OWN 128-root guard
    // (54495) while the `decode_block_128rect` row names
    // `read_sb128_root`, so deleting the inter copy also left it green.
    // The merged scan's count cross-check is what reds them, and a
    // count is not a location.
    //
    // So each row below names an ANCHOR that is unique in the decode
    // body -- the code the site hangs off -- and requires the level's
    // guard to sit between that anchor and the dispatch the site
    // feeds, within the same function. A level guard elsewhere in that
    // function cannot stand in, because the search starts AT the
    // anchor. Deleting the named guard finds no guard after the anchor
    // and reds; moving it below the dispatch reds the ordering assert.
    //
    // Row shape: (site label, guard's function, level, unique anchor,
    // dispatch token the guard must precede, shapes the site walks).
    let body_of = |name: &str| -> &str {
        let start = decode_body
            .find(&format!("\nfn {name}("))
            .or_else(|| decode_body.find(&format!("\npub(crate) fn {name}(")))
            .or_else(|| decode_body.find(&format!("\npub fn {name}(")))
            .unwrap_or_else(|| panic!("{name} is not a function in this file"));
        let end = ["\nfn ", "\npub fn ", "\npub(crate) fn "]
            .iter()
            .filter_map(|pat| decode_body[start + 1..].find(pat).map(|i| start + 1 + i))
            .min()
            .unwrap_or(decode_body.len());
        &decode_body[start..end]
    };
    let site_rows: &[(&str, &str, usize, &str, &str, &[(usize, usize)])] = &[
        // The key-frame 8x8 level resolves TWICE (37322 for the frame
        // interior, 37621 for the `pre8` probe path). Both feed
        // `decode_leaf_rect8` on a raw `part8`, whose VERT shape is 4x8 --
        // BLOCK_INVALID at 4:2:2. The anchor `let pre8 = ...` is what
        // makes this row about the SECOND site: with `find()` scoped to
        // the function alone the search would return 37322 and never
        // look here.
        (
            "the key-frame 8x8 SECOND resolution",
            KF,
            8,
            "let pre8 = dec.debug_state().0;",
            "decode_leaf_rect8(",
            &[(8, 4), (4, 8)],
        ),
        // The inter decoder resolves its own 128 root at 54495 rather
        // than routing through `read_sb128_root` (which is where the
        // `decode_block_128rect` row's guard lives). The dispatch token
        // is the `half128_inter!` arm that descends into a 128x64 /
        // 64x128 rect -- both BLOCK_INVALID at 4:2:2 in
        // `PLANE_BLOCK_INVALID_422`. `macro_rules! half128_inter` is
        // unique in the file, so this row cannot drift onto the other
        // one.
        (
            "the inter decoder's own 128-root guard",
            INTER,
            128,
            "macro_rules! half128_inter",
            "half128_inter!((br_mi, base_mi.1), 128, 64,",
            &[(128, 64), (64, 128)],
        ),
    ];
    for (label, fscope, level, anchor, dispatch, shapes) in site_rows {
        assert_eq!(
            decode_body.matches(anchor).count(),
            1,
            "{label}: the site anchor {anchor:?} is not unique in the decode body, so it \
             cannot name one site -- re-derive the anchor for this row"
        );
        // Why the site needs a guard at all: something it dispatches is
        // BLOCK_INVALID at a mode a `color_config` can select. (0, 1) is
        // excluded for the same reason as the walk census above.
        let offending: Vec<(usize, usize, usize, usize)> = shapes
            .iter()
            .flat_map(|&(bw, bh)| {
                [(1, 0), (1, 1), (0, 0)]
                    .into_iter()
                    .filter_map(move |(sx, sy)| {
                        invalid_at(bw, bh, sx, sy).then_some((bw, bh, sx, sy))
                    })
            })
            .collect();
        assert!(
            !offending.is_empty(),
            "{label} claims a guard but libaom codes every shape it walks at every \
             selectable subsampling mode -- then it needs none"
        );
        let body = body_of(fscope);
        let a = body.find(anchor).unwrap_or_else(|| {
            panic!(
                "{label}: the anchor {anchor:?} is not inside `{fscope}` -- the guard \
                 moved to another function, and the row's scope is stale"
            )
        });
        let gpat = format!("refuse_invalid_subsize(({level}, {level})");
        let g = body[a..].find(&gpat).map(|i| a + i).unwrap_or_else(|| {
            panic!(
                "{label}: `{fscope}` has NO {gpat} at or after the site anchor {anchor:?} \
                 -- this site's guard is gone. The level claim elsewhere in this test is \
                 still satisfied by the OTHER {level}x{level} guard in the file, which is \
                 precisely why this row exists"
            )
        });
        let gap = body[a..g].lines().count();
        assert!(
            gap <= 40,
            "{label}: the {gpat} sits {gap} lines below its own anchor {anchor:?} -- that is \
             a different site, so this row has stopped naming what it claims to name"
        );
        let d = body[g..].find(dispatch).map(|i| g + i).unwrap_or_else(|| {
            panic!(
                "{label}: `{fscope}` dispatches {dispatch:?} nowhere AFTER its {gpat} -- \
                 either the dispatch moved above the guard or the site was rewritten"
            )
        });
        assert!(
            g < d,
            "{label}: guard at {g} sits at or after the dispatch at {d} -- a guard BELOW \
             the dispatch can never observe it"
        );
    }
}

/// lane/av1422rect4dead: the `(pw / 2, ph / 2)` / `MI / 2` origin `else`
/// arm of [`decode_rect4_16_intrabc`] is the 4:2:0 PAIR geometry. It is
/// still LIVE at ss (1, 1) -- a 1:4 strip's chroma lies inside its
/// 4:2:0 pair there -- so the arm is not deleted; it is CLOSED at ss
/// (1, 0), where it would run on a 4x16 VERT_4 strip whose chroma plane
/// block is BLOCK_INVALID, and where libaom's own
/// `is_chroma_reference` (av1_common_int.h:1454, `|| !subsampling_y`)
/// makes every strip its own chroma reference anyway (the `own422` arm
/// handles that). The proof: the sole caller `decode_rect4_16_strip` is
/// reached only via the two 16x16-level 1:4 arms, each under
/// `refuse_invalid_subsize((16, 16), part16)` -- at ss (1, 0) that guard
/// refuses `partition_subsize_dims((16, 16), PARTITION_VERT_4) == (4, 16)`
/// (in `PLANE_BLOCK_INVALID_422`), and the surviving HORZ_4 16x4 strip
/// takes `own422`. This test re-derives that chain from the file so it
/// REDS when any half stops holding: deleting the in-arm assertion, a
/// caller moving under a level whose guard does not refuse (4, 16) at
/// ss (1, 0), or the `(pw / 2, ph / 2)` form drifting. 4:2:0 and 4:4:4
/// behaviour are unchanged: the gate runs only the file scan and the
/// closed cell's arithmetic, no decode.
#[test]
fn decode_rect4_16_intrabc_else_arm_stays_closed_at_422() {
    let production = crate::test_source::module_production_source("decode");
    let decode_body = match production.find("\n#[cfg(test)]\nmod tests {") {
        Some(i) => &production[..i],
        None => production,
    };

    // (a) The closure assertion must exist INSIDE the function, in the
    // `else` arm of the `(cw, ch)` select, naming the shape and the
    // subsampling. Deleting it -- or re-adding the bare halving without
    // it -- reds here.
    let fstart = decode_body
        .find("\nfn decode_rect4_16_intrabc(")
        .expect("decode_rect4_16_intrabc is gone -- this gate guards its else arm");
    let fend = ["\nfn ", "\npub fn ", "\npub(crate) fn "]
        .iter()
        .filter_map(|pat| decode_body[fstart + 1..].find(pat).map(|i| fstart + 1 + i))
        .min()
        .unwrap_or(decode_body.len());
    let body = &decode_body[fstart..fend];
    let else_arm = body
        .find("    let (cw, ch) = if own_chroma {")
        .expect("the (cw, ch) select moved out of decode_rect4_16_intrabc");
    let closure = &body[else_arm..];
    assert!(
        closure.contains("assert!(") && closure.contains("ss_x(fctx) == 1 && ss_y(fctx) == 0"),
        "the decode_rect4_16_intrabc else arm's ss (1, 0) closure assertion is \
         GONE -- the 4:2:0 pair geometry is unreachable at ss (1, 0) only while \
         that assert (and the guards below) stand; re-add it"
    );

    // (b) The sole-caller chain, re-derived from the file: exactly one
    // call to `decode_rect4_16_intrabc(` outside its own definition, in
    // `decode_rect4_16_strip`; exactly two calls to
    // `decode_rect4_16_strip(` from the two 1:4 routes.
    let code_lines: String = decode_body
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let count_calls = |name: &str| -> usize {
        code_lines.matches(&format!("{name}(")).count()
            - usize::from(code_lines.contains(&format!("fn {name}(")))
    };
    assert_eq!(
        count_calls("decode_rect4_16_intrabc"),
        1,
        "decode_rect4_16_intrabc gained a caller -- re-derive whether the new route \
         sits under a guard that refuses (4, 16) at ss (1, 0) before this gate \
         stays green"
    );
    assert_eq!(
        count_calls("decode_rect4_16_strip"),
        2,
        "decode_rect4_16_strip's caller set moved -- both routes must be \
         16x16-level 1:4 arms under a (16, 16) guard that refuses (4, 16) at \
         ss (1, 0)"
    );

    // (c) Both routes sit under a `refuse_invalid_subsize((16, 16), ...)`:
    // the key-frame arm's guard ordered above the arm test, the inter
    // arm's guard in the inter decoder above its own 1:4 test. Scoped to
    // the enclosing function so the other decoder's guard cannot stand in
    // (the r2 class this file already fixed once).
    let fn_scope = |at: usize| -> std::ops::Range<usize> {
        let head = ["\nfn ", "\npub fn ", "\npub(crate) fn "]
            .iter()
            .filter_map(|pat| decode_body[..at].rfind(pat))
            .max()
            .unwrap_or(0);
        let tail = ["\nfn ", "\npub fn ", "\npub(crate) fn "]
            .iter()
            .filter_map(|pat| decode_body[at..].find(pat).map(|i| at + i))
            .min()
            .unwrap_or(decode_body.len());
        head..tail
    };
    let kf_test = decode_body
        .find("if part16 == PARTITION_HORZ_4")
        .expect("the key-frame 16-level 1:4 arm test is gone");
    let kf_scope = fn_scope(kf_test);
    let kf_guard = decode_body[kf_scope.clone()]
        .find("refuse_invalid_subsize((16, 16)")
        .map(|i| kf_scope.start + i)
        .unwrap_or_else(|| {
            panic!(
                "the key-frame 16-level 1:4 arm has NO \
                 refuse_invalid_subsize((16, 16)) above it in its own \
                 function -- the VERT_4 (4, 16) refusal that closed \
                 decode_rect4_16_intrabc's else arm at ss (1, 0) is gone"
            )
        });
    assert!(
        kf_guard < kf_test,
        "the key-frame (16, 16) guard sits below the 1:4 arm test -- it can never \
         observe the VERT_4 symbol that must be refused before \
         decode_rect4_16_intrabc's else arm can run at ss (1, 0)"
    );
    let inter_writer = decode_body
        .find("let horz = part16 == PARTITION_HORZ_4;")
        .expect("the inter 16-level 1:4 arm is gone");
    let inter_scope = fn_scope(inter_writer);
    let inter_guard = decode_body[inter_scope.clone()]
        .find("refuse_invalid_subsize((16, 16)")
        .map(|i| inter_scope.start + i)
        .unwrap_or_else(|| {
            panic!(
                "the inter-frame 16-level 1:4 arm has NO \
                 refuse_invalid_subsize((16, 16)) in its function -- the VERT_4 \
                 (4, 16) refusal that closed decode_rect4_16_intrabc's else arm \
                 at ss (1, 0) is gone"
            )
        });
    assert!(
        inter_guard < inter_writer,
        "the inter (16, 16) guard sits below the 1:4 arm test -- it can never \
         observe the VERT_4 symbol that must be refused before \
         decode_rect4_16_intrabc's else arm can run at ss (1, 0)"
    );

    // (d) The shape/subsampling cells the closure is written against,
    // re-executed: (4, 16) must still be BLOCK_INVALID at ss (1, 0) while
    // (16, 4) stays codable there, and at ss (1, 1) BOTH are codable --
    // which is what keeps the arm live at 4:2:0 and the assertion inert
    // there. At ss (0, 0) everything is codable, so `own444` (and the
    // assertion) never see the else arm.
    assert!(
        !chroma_plane_block_codable(4, 16, 1, 0),
        "(4, 16) left PLANE_BLOCK_INVALID_422 -- the else arm is now REACHABLE at \
         ss (1, 0) with the halved pair geometry, which is a real defect, not a \
         closure: re-derive the whole chain, then fix the halving instead of the \
         assert"
    );
    assert!(
        chroma_plane_block_codable(16, 4, 1, 0),
        "(16, 4) became BLOCK_INVALID at ss (1, 0) -- the surviving HORZ_4 strip \
         takes own422, and the closure assert would fire on a legal shape; \
         re-derive"
    );
    assert!(
        chroma_plane_block_codable(4, 16, 1, 1) && chroma_plane_block_codable(16, 4, 1, 1),
        "a 1:4 strip's plane block moved at ss (1, 1) -- the else arm is LIVE there \
         and the pair geometry is what decodes it; the assertion must stay inert at \
         4:2:0"
    );
    assert!(
        chroma_plane_block_codable(4, 16, 0, 0) && chroma_plane_block_codable(16, 4, 0, 0),
        "a 1:4 strip's plane block became invalid at ss (0, 0) -- the assertion sits \
         after the own444 arm and must never fire at 4:4:4; re-derive"
    );
}

/// lane-av1chrtx: [`covering_leaf_tx_type`] resolves a chroma unit's
/// `tx_type` from the luma leaf covering the unit's OWN quadrant, and
/// reports "no leaf there" as `None` so the caller can fall back to the
/// block-level type. Both halves are load-bearing: the 128 mu-chunk tail,
/// the 4:4:4 four-unit arms and `read_inter_rect_chroma` all read the
/// `None` arm as "this block coded a single luma unit".
#[test]
fn covering_leaf_tx_type_picks_the_leaf_under_the_unit_or_reports_none() {
    use TxType::*;
    // A 32x64 strip's five-leaf tree, in decode order: one 32x32 leaf
    // over mi rows 0..8, then 16x16 leaves. (row, col) are MI relative
    // to the block, (tw, th) are PIXELS, MI == 4.
    let leaves = [
        (0usize, 0usize, 32usize, 32usize, Idtx),
        (8, 0, 16, 16, VDct),
        (8, 4, 16, 16, DctDct),
        (12, 0, 16, 16, HDct),
        (12, 4, 16, 16, VDct),
    ];
    // The TOP chroma unit (its own mi cell (0,0)) sits over the 32x32
    // leaf; the LOWER one (mi (8,0)) over the first 16x16 leaf. The
    // block-level type is the first leaf's, so only the second read
    // differs -- exactly the condition the gate's DIFF counter counts.
    assert_eq!(covering_leaf_tx_type(&leaves, (0, 0)), Some(Idtx));
    assert_eq!(covering_leaf_tx_type(&leaves, (8, 0)), Some(VDct));
    // Interior cells resolve the same leaf as the cell's top-left: the
    // lookup is a rectangle containment test, not a raster position.
    assert_eq!(covering_leaf_tx_type(&leaves, (11, 3)), Some(VDct));
    assert_eq!(covering_leaf_tx_type(&leaves, (12, 4)), Some(VDct));
    // The bottom-right corner of the last leaf, and the mi cell just
    // past the block's own extent, must not fall into a neighbour.
    assert_eq!(covering_leaf_tx_type(&leaves, (15, 7)), Some(VDct));
    assert_eq!(covering_leaf_tx_type(&leaves, (16, 0)), None);
    assert_eq!(covering_leaf_tx_type(&leaves, (0, 8)), None);
    // A block with no var-tx tree (one coded luma unit) has no leaves at
    // all: the caller's block-level fallback is the answer, and the
    // resolver must say so rather than guess.
    assert_eq!(covering_leaf_tx_type(&[], (0, 0)), None);
}

/// lane-av1422warp r33: the OBMC pair-merge snap must move the READ to
/// libaom's position, not the reported offset.
///
/// libaom `obmc.h:44-46`:
/// ```c
/// if (mi_step == 1) {
///   above_mi_col &= ~1;                        // snap the INDEX, unconditional
///   above_mi = prev_row_mi + above_mi_col + 1; // read at snap + 1
///   mi_step = 2;
/// }
/// ```
/// and the relative offset handed to
/// `av1_setup_build_prediction_by_above_pred` is
/// `above_mi_col - mi_col` **after** the snap, which is `-1` at an odd
/// `mi_col`. An earlier fix clamped the snapped INDEX to `mi_col` to
/// stop that subtraction underflowing, which silenced the underflow and
/// in exchange read the neighbour one column RIGHT of libaom's and then
/// stepped past the block's own right edge.
///
/// The grid here carries DISTINGUISHABLE neighbours at the pair's even
/// half, its odd half, and the column to the right of the pair, so the
/// test fails if the walk reads any position but the odd half. It is the
/// snapped READ this pins -- and no stream in the corpus exercises it
/// (a1.obu's 1338 traced OBMC neighbours are byte-identical before and
/// after the fix, because its 4-wide pair never starts on an odd edge),
/// so nothing else covers this branch.
#[test]
fn the_obmc_pair_merge_snap_reads_libaoms_neighbour_not_one_to_the_right() {
    const NAME: &str = "the_obmc_pair_merge_snap_reads_libaoms_neighbour_not_one_to_the_right";
    // `ref_frame` 1 == LAST_FRAME, which is what `is_inter` consumers
    // expect; the MV makes each cell individually identifiable.
    let cell = |mv: (i16, i16), size: u8| MiInfo {
        is_inter: true,
        ref_frame: 1,
        ref_frame1: 0,
        mv,
        mv1: (0, 0),
        is_new_mv: true,
        size,
        size_h: size,
        is_global_mv0: false,
        is_global_mv1: false,
    };
    const EVEN: (i16, i16) = (10, 10);
    const ODD: (i16, i16) = (20, 20);
    const RIGHT: (i16, i16) = (30, 30);

    // --- the above walk, at an ODD `mi_col` ---
    let mut grid = crate::mvstack::MiGrid::new(16, 16);
    // The block under test sits at mi_col 5; the neighbour row is 4.
    // A 4-wide inter cell at the ODD half (5) is what forces
    // `mi_step == 1` and therefore the pair merge.
    grid.set(4, 4, cell(EVEN, 1));
    grid.set(4, 5, cell(ODD, 1));
    grid.set(4, 6, cell(RIGHT, 4)); // one column RIGHT of the pair
    // `bw4 == 2` so `AOMMIN(xd->width, mi_step)` is the pair's own 2
    // rather than clipped to a 1-wide block's 1.
    let nb = overlappable_above(&grid, 5, 5, 2, 16, 4);
    assert!(
        !nb.is_empty(),
        "{NAME}: the above walk found no neighbour at all"
    );
    let (off4, span4, info, src4) = nb[0];
    assert_eq!(
        info.mv, ODD,
        "{NAME}: the above pair merge read the wrong neighbour -- libaom \
         snaps the column and reads snap+1, which is the ODD half"
    );
    assert_ne!(
        info.mv, RIGHT,
        "{NAME}: the clamp moved the snapped index onto mi_col, so the \
         walk read one column RIGHT of libaom"
    );
    assert_eq!(src4, 0, "{NAME}: the read column's relative offset");
    assert_eq!(span4, 2, "{NAME}: the pair steps over BOTH halves");
    // libaom's `above_mi_col - mi_col` is -1 here; this tuple is
    // `usize`, so the reported offset saturates to 0 rather than
    // wrapping to `usize::MAX`. The snap is what the read depends on.
    assert_eq!(
        off4, 0,
        "{NAME}: the reported offset must saturate, not wrap"
    );

    // --- the left walk, at an ODD `mi_row`: the same shape ---
    let mut grid = crate::mvstack::MiGrid::new(16, 16);
    grid.set(4, 4, cell(EVEN, 1));
    grid.set(5, 4, cell(ODD, 1));
    grid.set(6, 4, cell(RIGHT, 4));
    let nb = overlappable_left(&grid, 5, 5, 2, 16, 4);
    assert!(
        !nb.is_empty(),
        "{NAME}: the left walk found no neighbour at all"
    );
    let (off4, span4, info, src4) = nb[0];
    assert_eq!(
        info.mv, ODD,
        "{NAME}: the left pair merge read the wrong neighbour -- libaom \
         snaps the row and reads snap+1, which is the ODD half"
    );
    assert_ne!(
        info.mv, RIGHT,
        "{NAME}: the clamp moved the snapped row onto mi_row, so the walk \
         read one row BELOW libaom"
    );
    assert_eq!(src4, 0, "{NAME}: the read row's relative offset");
    assert_eq!(span4, 2, "{NAME}: the pair steps over BOTH halves");
    assert_eq!(
        off4, 0,
        "{NAME}: the reported offset must saturate, not wrap"
    );
}

/// lane-twostage: the reference bank these gates decode against --
/// `LAST_FRAME` alone, already in hand (`ref_frame` index 1).
fn last_only(reference: &Picture) -> RefPix<'_> {
    let mut refs: [Option<&Picture>; 8] = [None; 8];
    refs[1] = Some(reference);
    RefPix::ready(refs)
}

/// lane-av1422stripwrite: the reference chroma shape is read back off the
/// stored plane's own sample count, so the counts the PRODUCER writes
/// (`round_ss` crops, `decode.rs:25806`) must route back to the shape
/// they came from. A floor half-width here passes every even-width case
/// -- the pinned fixture's 256-wide frame included -- and silently drops
/// every odd-width 4:2:2 reference into the 4:2:0 arm, which is the
/// exact defect this lane fixed, one dimension over. So the table below
/// is driven off `round_ss`, not off hand-written numbers, and sweeps
/// odd and even widths.
///
/// lane-av1odd440: the 4:2:0 arm's expectation is the PRODUCER's crop,
/// `round_ss(w, 1) x round_ss(h, 1)`, not the `(w / 2, h / 2)` floor it
/// used to name -- the floor under-declares every odd-dimension 4:2:0
/// reference, which is the shape the pinned `420_odd65x65_key.obu` cell
/// finally makes reachable (see [`ref_chroma_shape`]). For every EVEN
/// size the two spellings are the same number, so the arms that were
/// already correct are unchanged by the edit.
#[test]
fn a_reference_chroma_sample_count_routes_back_to_its_own_format() {
    for w in [2usize, 3, 4, 5, 8, 9, 16, 17, 64, 65, 128, 129] {
        for h in [2usize, 3, 4, 5, 8, 9, 64, 65, 144, 145] {
            let cases = [
                ("4:4:4", round_ss(w, 0) * round_ss(h, 0), (w, h)),
                (
                    "4:2:2",
                    round_ss(w, 1) * round_ss(h, 0),
                    (round_ss(w, 1), h),
                ),
                (
                    "4:2:0",
                    round_ss(w, 1) * round_ss(h, 1),
                    (round_ss(w, 1), round_ss(h, 1)),
                ),
            ];
            for (name, len, want) in cases {
                assert_eq!(
                    ref_chroma_shape(len, w, h),
                    want,
                    "{name} reference of {w}x{h} (chroma {len} samples) \
                     must route back to its own shape"
                );
            }
        }
    }
}

/// lane-av1422stripwrite: the whole point of the 4:2:2 arm is that it
/// claims the frame's FULL chroma height -- that is what stops every
/// chroma MC sourcing a row at or past the midpoint from being clamped
/// against `true_height`. Pinned on the t422 shape.
#[test]
fn a_422_reference_claims_the_full_chroma_height() {
    // 256x144 luma at ss (1, 0): chroma is 128 wide and 144 rows tall.
    let (cw, ch) = ref_chroma_shape(round_ss(256, 1) * 144, 256, 144);
    assert_eq!((cw, ch), (128, 144));
    // The bug's own signature: 72 rows claimed for 144 rows held.
    assert!(
        ch > 144 / 2,
        "a 4:2:2 reference must not claim half its rows"
    );
}

/// [`last_only`] plus `GOLDEN_FRAME` (index 4 of the name-keyed array)
/// and `ALTREF_FRAME` (index 7, the frame two back, which the encoder
/// keeps in the slot the current frame refreshes): what a frame this
/// encoder writes needs, since its blocks may name any of the three.
fn last_golden_altref<'a>(
    last: &'a Picture,
    golden: &'a Picture,
    altref: Option<&'a Picture>,
) -> RefPix<'a> {
    let mut refs: [Option<&Picture>; 8] = [None; 8];
    refs[1] = Some(last);
    refs[4] = Some(golden);
    refs[7] = altref;
    RefPix::ready(refs)
}

/// lane-perf6 step 2: the CDEF kernel now reads a pre-gathered window and
/// a pre-resolved damping shift instead of a bounds-testing closure and a
/// per-tap `msb`. Both replacements must be value-identical to the spec
/// forms they came from, over the whole strength/damping/diff domain.
#[test]
fn cdef_gather_and_shifted_constrain_match_the_spec_forms() {
    for damping in 3..=9i32 {
        for threshold in 1..=64i32 {
            let shift = super::cdef_constrain_shift(threshold, damping);
            for diff in -300..=300i32 {
                assert_eq!(
                    super::cdef_constrain(diff, threshold, damping),
                    super::cdef_constrain_shifted(diff, threshold, shift),
                    "diff={diff} threshold={threshold} damping={damping}"
                );
            }
        }
    }
    // `cdef_gather`'s interior fast path and its bounds-testing slow path
    // must agree with the per-sample closure the filter used to call, at
    // every block position including the three that straddle the crop.
    let (stride, tw, th) = (24usize, 21usize, 19usize);
    let src: Vec<u16> = (0..stride * 24).map(|i| (i * 7 % 1024) as u16).collect();
    for &(ox, oy, bw, bh) in &[(0, 0, 8, 8), (8, 8, 8, 8), (16, 16, 8, 8), (4, 12, 4, 4)] {
        let bstride = bw + 2 * super::CDEF_BORDER;
        let mut win = vec![0i16; bstride * (bh + 2 * super::CDEF_BORDER)];
        super::cdef_gather(&src, stride, tw, th, ox, oy, bw, bh, &mut win);
        for r in -(super::CDEF_BORDER as i32)..(bh + super::CDEF_BORDER) as i32 {
            for c in -(super::CDEF_BORDER as i32)..(bw + super::CDEF_BORDER) as i32 {
                let (ny, nx) = (oy as i32 + r, ox as i32 + c);
                let want = if ny < 0 || nx < 0 || ny as usize >= th || nx as usize >= tw {
                    super::CDEF_VERY_LARGE as i16
                } else {
                    src[ny as usize * stride + nx as usize] as i16
                };
                let got = win[(r + super::CDEF_BORDER as i32) as usize * bstride
                    + (c + super::CDEF_BORDER as i32) as usize];
                assert_eq!(got, want, "({ox},{oy}) r={r} c={c}");
            }
        }
    }
}

/// lane-cdef: the row-at-a-time direction search (and its AVX2 twin) must
/// be value-identical to a literal transcription of libaom
/// `cdef_find_dir_c`'s per-sample accumulation, over random windows at
/// every bit depth -- including the `CDEF_VERY_LARGE` sentinel rows a
/// crop-straddling 8x8 gathers.
#[test]
fn cdef_find_dir_matches_the_per_sample_reference() {
    const DIV_TABLE: [i64; 9] = [0, 840, 420, 280, 210, 168, 140, 120, 105];
    fn reference(win: &[i16], bstride: usize, coeff_shift: i32) -> (usize, i32) {
        let mut partial = [[0i64; 15]; 8];
        for i in 0..8i64 {
            for j in 0..8i64 {
                let s = i64::from(
                    win[(i as usize + super::CDEF_BORDER) * bstride
                        + j as usize
                        + super::CDEF_BORDER],
                );
                let x = (s >> coeff_shift) - 128;
                partial[0][(i + j) as usize] += x;
                partial[1][(i + j / 2) as usize] += x;
                partial[2][i as usize] += x;
                partial[3][(3 + i - j / 2) as usize] += x;
                partial[4][(7 + i - j) as usize] += x;
                partial[5][(3 - i / 2 + j) as usize] += x;
                partial[6][j as usize] += x;
                partial[7][(i / 2 + j) as usize] += x;
            }
        }
        let mut cost = [0i64; 8];
        // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
        #[allow(clippy::needless_range_loop)]
        for i in 0..8usize {
            cost[2] += partial[2][i] * partial[2][i];
            cost[6] += partial[6][i] * partial[6][i];
        }
        cost[2] *= DIV_TABLE[8];
        cost[6] *= DIV_TABLE[8];
        for i in 0..7usize {
            cost[0] += (partial[0][i] * partial[0][i] + partial[0][14 - i] * partial[0][14 - i])
                * DIV_TABLE[i + 1];
            cost[4] += (partial[4][i] * partial[4][i] + partial[4][14 - i] * partial[4][14 - i])
                * DIV_TABLE[i + 1];
        }
        cost[0] += partial[0][7] * partial[0][7] * DIV_TABLE[8];
        cost[4] += partial[4][7] * partial[4][7] * DIV_TABLE[8];
        for i in (1..8).step_by(2) {
            for j in 0..5usize {
                cost[i] += partial[i][3 + j] * partial[i][3 + j];
            }
            cost[i] *= DIV_TABLE[8];
            for j in 0..3usize {
                cost[i] += (partial[i][j] * partial[i][j]
                    + partial[i][10 - j] * partial[i][10 - j])
                    * DIV_TABLE[2 * j + 2];
            }
        }
        let (mut best_cost, mut best_dir) = (0i64, 0usize);
        for (i, &c) in cost.iter().enumerate() {
            if c > best_cost {
                best_cost = c;
                best_dir = i;
            }
        }
        (
            best_dir,
            ((best_cost - cost[(best_dir + 4) & 7]) >> 10) as i32,
        )
    }
    let mut rng = 0x0bad_c0de_dead_beefu64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let bstride = 8 + 2 * super::CDEF_BORDER;
    for &(bit_depth, coeff_shift) in &[(8u8, 0i32), (10, 2), (12, 4)] {
        let smax = (1u64 << bit_depth) - 1;
        for trial in 0..256 {
            let sentinels = trial % 4;
            let mut win = [0i16; 256];
            for (k, w) in win[..bstride * bstride].iter_mut().enumerate() {
                let (r, c) = (k / bstride, k % bstride);
                *w = if sentinels > 0
                    && (r < sentinels
                        || c < sentinels
                        || r + sentinels >= bstride
                        || c + sentinels >= bstride)
                {
                    super::CDEF_VERY_LARGE as i16
                } else {
                    (next() % (smax + 1)) as i16
                };
            }
            let want = reference(&win, bstride, coeff_shift);
            assert_eq!(
                super::cdef_find_dir(&win, bstride, coeff_shift),
                want,
                "bd={bit_depth} trial={trial}"
            );
            let partial = super::cdef_find_dir_partials(&win, bstride, coeff_shift);
            #[cfg(target_arch = "x86_64")]
            if crate::mc::simd_level() == crate::mc::SimdLevel::Avx2 {
                let simd = super::cdef::cdef_find_dir_partials_dispatch(&win, bstride, coeff_shift);
                assert_eq!(simd, partial, "bd={bit_depth} trial={trial} partials");
            }
        }
    }
}

/// The CDEF filter through a `multiversion` clone, so the AVX2 kernel is
/// reached by a safe call: the AVX2 clone runs `filter_block_avx2`, the
/// baseline clone the scalar form (the same split `cdef_filter_block` uses).
#[cfg(target_arch = "x86_64")]
#[allow(clippy::too_many_arguments)]
#[multiversion::multiversion(targets("x86_64+avx2"))]
fn cdef_filter_block_under_test(
    src: &[i16],
    dst: &mut [u16],
    stride: usize,
    ox: usize,
    oy: usize,
    bw: usize,
    bh: usize,
    pri_strength: i32,
    sec_strength: i32,
    dir: usize,
    pri_damping: i32,
    sec_damping: i32,
    enable_primary: bool,
    enable_secondary: bool,
    coeff_shift: i32,
    smax: i32,
) {
    multiversion::target::match_target! {
        "x86_64+avx2" => super::cdef_simd::filter_block_avx2(
            src, dst, stride, ox, oy, bw, bh, pri_strength, sec_strength, dir,
            pri_damping, sec_damping, enable_primary, enable_secondary, coeff_shift, smax,
        ),
        _ => super::cdef_filter_block_scalar(
            src, dst, stride, ox, oy, bw, bh, pri_strength, sec_strength, dir,
            pri_damping, sec_damping, enable_primary, enable_secondary, coeff_shift, smax,
        ),
    }
}

/// lane-perf7 step 1: the AVX2 CDEF kernel must be value-identical to
/// [`super::cdef_filter_block_scalar`] over the whole input domain --
/// every direction, every enable pair, both block shapes, the 8/10/12-bit
/// strength and damping ranges, and windows that mix real samples with
/// the `CDEF_VERY_LARGE` out-of-frame sentinel (which must stay out of
/// the max clamp).
#[test]
#[cfg(target_arch = "x86_64")]
fn cdef_simd_kernel_matches_the_scalar_filter() {
    if crate::mc::simd_level() != crate::mc::SimdLevel::Avx2 {
        eprintln!("SKIP cdef simd: no avx2");
        return;
    }
    let mut rng = 0x1234_5678_9abc_def0u64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    for &(bit_depth, coeff_shift) in &[(8u8, 0i32), (10, 2), (12, 4)] {
        let smax = (1i32 << bit_depth) - 1;
        for &(bw, bh) in &[(8usize, 8usize), (4, 4)] {
            let bstride = bw + 2 * super::CDEF_BORDER;
            let rows = bh + 2 * super::CDEF_BORDER;
            let stride = bw + 5;
            for trial in 0..64 {
                // Every fourth window seeds sentinels, in a growing
                // border ring, so the straddling-block case is covered.
                let sentinels = trial % 4;
                // 256 entries, as `cdef_band`'s own scratch is: the
                // kernel taps the window in place and reads past the last
                // sample into the zeroed tail.
                let mut win: Vec<i16> = (0..rows * bstride)
                    .map(|k| {
                        let (r, c) = (k / bstride, k % bstride);
                        if sentinels > 0
                            && (r < sentinels
                                || c < sentinels
                                || r + sentinels >= rows
                                || c + sentinels >= bstride)
                        {
                            super::CDEF_VERY_LARGE as i16
                        } else {
                            (next() % (smax as u64 + 1)) as i16
                        }
                    })
                    .collect();
                win.resize(256, 0);
                for dir in 0..8usize {
                    for &pri in &[0i32, 1, 3, 15] {
                        for &sec in &[0i32, 1, 2, 4] {
                            for &damping in &[3i32, 5, 6, 9] {
                                let (pri, sec) = (pri << coeff_shift, sec << coeff_shift);
                                let (ep, es) = (pri != 0, sec != 0);
                                if !ep && !es {
                                    continue;
                                }
                                let seed: Vec<u16> =
                                    (0..stride * (bh + 2)).map(|k| (k % 97) as u16).collect();
                                let mut a = seed.clone();
                                let mut b = seed;
                                super::cdef_filter_block_scalar(
                                    &win,
                                    &mut a,
                                    stride,
                                    1,
                                    1,
                                    bw,
                                    bh,
                                    pri,
                                    sec,
                                    dir,
                                    damping,
                                    damping,
                                    ep,
                                    es,
                                    coeff_shift,
                                    smax,
                                );
                                cdef_filter_block_under_test(
                                    &win,
                                    &mut b,
                                    stride,
                                    1,
                                    1,
                                    bw,
                                    bh,
                                    pri,
                                    sec,
                                    dir,
                                    damping,
                                    damping,
                                    ep,
                                    es,
                                    coeff_shift,
                                    smax,
                                );
                                assert_eq!(
                                    a, b,
                                    "bd={bit_depth} bw={bw} trial={trial} dir={dir} \
                                     pri={pri} sec={sec} damping={damping}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

/// lane-perf8: the AVX2 deblock kernel must be sample-identical to
/// [`super::filter_edge`] over the whole input domain -- every bit depth,
/// every filter length, every level and sharpness, and sample patterns
/// that put each of `mask`, `flat` and `flat2` on both sides of its
/// threshold (uniform, flat-with-step, small-noise and full-range
/// random), on both edge directions.
#[test]
#[cfg(target_arch = "x86_64")]
fn deblock_simd_kernel_matches_the_scalar_filter() {
    let fctx = &crate::decode::FrameCtx::new();
    if crate::mc::simd_level() != crate::mc::SimdLevel::Avx2 {
        eprintln!("SKIP deblock simd: no avx2");
        return;
    }
    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let (stride, rows) = (64usize, 64usize);
    for &bd in &[8u8, 10, 12] {
        super::set_bit_depth(bd, fctx);
        let smax = (1u64 << bd) - 1;
        for pattern in 0..4u32 {
            for trial in 0..8 {
                let seed: Vec<u16> = (0..stride * rows)
                    .map(|k| {
                        let base = ((k / stride) as u64 * 7 + trial) % (smax + 1);
                        (match pattern {
                            // Uniform-ish: every mask and both flats pass.
                            0 => base,
                            // A step across the edge column/row.
                            1 => (base + if k % stride >= 32 { smax / 2 } else { 0 }) % (smax + 1),
                            // Small noise: flat passes at 8-bit, not always higher.
                            2 => (base + next() % 4) % (smax + 1),
                            _ => next() % (smax + 1),
                        }) as u16
                    })
                    .collect();
                for &len in &[4u8, 6, 8, 14] {
                    for &level in &[0i32, 1, 7, 23, 40, 63] {
                        for sharpness in 0..8u8 {
                            for &vertical in &[true, false] {
                                let (base, outer, tap) = if vertical {
                                    (32 * stride + 32, stride as isize, 1isize)
                                } else {
                                    (32 * stride + 32, 1isize, stride as isize)
                                };
                                let mut a = seed.clone();
                                for g in 0..4usize {
                                    let b = if vertical {
                                        base + 4 * g * stride
                                    } else {
                                        base + 4 * g
                                    };
                                    super::filter_edge(
                                        &mut a, b, outer, tap, len, level, sharpness, fctx,
                                    );
                                }
                                let mut b = seed.clone();
                                assert!(super::filter_edge16(
                                    &mut b, base, stride, vertical, len, level, sharpness, fctx,
                                ));
                                assert_eq!(
                                    a, b,
                                    "bd={bd} pattern={pattern} trial={trial} len={len} \
                                     level={level} sharp={sharpness} vert={vertical}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    super::set_bit_depth(8, fctx);
}

/// lane-golomb r7: [`reduce_inherited_chroma_tx_type`] must reproduce
/// `av1_ext_tx_used[av1_get_ext_tx_set_type(uv_tx, 1, reduced)]`
/// (libaom `av1/common/blockd.h:1036`/`1097`) exactly -- the g35 residue
/// was one 64x64 inter superblock whose 32x32 chroma units are
/// `EXT_TX_SET_DCT_IDTX`, where IDTX survives and we forced `DCT_DCT`.
#[test]
fn an_inter_chroma_transform_narrows_its_inherited_tx_type_to_its_own_set() {
    let fctx = &crate::decode::FrameCtx::new();
    use TxType::*;
    // EXT_TX_SET_DCTONLY: `tx_size_sqr_up > TX_32X32`.
    set_inter_tx_mode(true, false, true, fctx);
    assert_eq!(reduce_inherited_chroma_tx_type(Idtx, 64, 64, fctx), DctDct);
    // EXT_TX_SET_DCT_IDTX at exactly TX_32X32 -- the residue's case.
    assert_eq!(reduce_inherited_chroma_tx_type(Idtx, 32, 32, fctx), Idtx);
    assert_eq!(
        reduce_inherited_chroma_tx_type(AdstAdst, 32, 32, fctx),
        DctDct
    );
    // EXT_TX_SET_DTT9_IDTX_1DDCT at `tx_size_sqr == TX_16X16`: the four
    // 1D ADST/FLIPADST types (TX_TYPES 12..15) drop out, the rest stay.
    assert_eq!(reduce_inherited_chroma_tx_type(HDct, 16, 16, fctx), HDct);
    assert_eq!(
        reduce_inherited_chroma_tx_type(FlipAdstAdst, 16, 16, fctx),
        FlipAdstAdst
    );
    assert_eq!(reduce_inherited_chroma_tx_type(HAdst, 16, 16, fctx), DctDct);
    // EXT_TX_SET_ALL16 below that (`tx_size_sqr < TX_16X16`, so a 16x8
    // chroma unit too).
    assert_eq!(reduce_inherited_chroma_tx_type(HAdst, 16, 8, fctx), HAdst);
    assert_eq!(
        reduce_inherited_chroma_tx_type(VFlipAdst, 8, 8, fctx),
        VFlipAdst
    );
    // `reduced_tx_set` collapses every inter set to DCT_IDTX.
    set_inter_tx_mode(true, true, true, fctx);
    assert_eq!(reduce_inherited_chroma_tx_type(Idtx, 8, 8, fctx), Idtx);
    assert_eq!(reduce_inherited_chroma_tx_type(HAdst, 8, 8, fctx), DctDct);
    set_inter_tx_mode(true, false, true, fctx);
}

/// lane-fistrip r1 (class `enumerate-the-table-domain`): every block
/// shape this decoder can hand [`filter_intra_size_class_rect`], against
/// `av1_filter_intra_allowed_bsize` (`block_size_wide <= 32 &&
/// block_size_high <= 32`) and `default_filter_intra_cdfs`
/// (entropymode.c) row by row. A class that exists must carry ITS OWN
/// bsize's default probability -- a wrong row is a silently different
/// symbol, not a decode error. The shapes with no class are listed here
/// with their reason so the residue is pinned rather than implied.
#[test]
fn filter_intra_classes_carry_their_own_libaom_default_row() {
    // `default_filter_intra_cdfs[bsize]`, BLOCK_SIZES_ALL order, as
    // (width, height, default, refused_by) for the shapes <= 32 on both
    // axes. The first three fields are GENERATED from the oracle by
    // `scripts/extract-filter-intra-cdfs.py` (it reads BLOCK_SIZES_ALL's
    // order from enums.h and the table from entropymode.c, so no hand
    // transcription can drift -- lane-fistrip r1 listed the four 1:4
    // shapes as 16384 out of exactly that kind of guess; their real rows
    // are 12770/10368/20229/18101). `refused_by` is `None` for a shape
    // this decoder's partition tree reaches today -- it must own a class,
    // and that class must carry exactly this row -- and the refusal
    // string that blocks the shape otherwise. Either way the row is
    // pinned HERE, so the round that lifts one of those refusals cannot
    // land a wrong CDF row for the shape it unlocks.
    let libaom: &[(usize, usize, u16, Option<&str>)] = &[
        (4, 4, 4621, None),
        (4, 8, 6743, None),
        (8, 4, 5893, None),
        (8, 8, 7866, None),
        (8, 16, 12551, None),
        (16, 8, 9394, None),
        (16, 16, 12408, None),
        (16, 32, 14301, None),
        (32, 16, 12756, None),
        (32, 32, 22343, None),
        // The 1:4 strips: a 16x16-level PARTITION_HORZ_4/VERT_4 gives
        // 4x16/16x4 (still refused by name before any leaf is coded),
        // a 32x32-level one 8x32/32x8.
        // lane-r14: the 16-level pair is coded now (`decode_rect4_16`),
        // owning classes 13/12.
        (4, 16, 12770, None),
        (16, 4, 10368, None),
        // lane-fi32x8 r1: the 32-level pair is coded (four 32x8/8x32
        // strips per `PARTITION_HORZ_4`/`VERT_4`) and now owns classes
        // 10/11; only the 16-level pair is still refused before any leaf.
        (8, 32, 20229, None),
        (32, 8, 18101, None),
    ];
    let mut classes_seen = std::collections::BTreeSet::new();
    for &(bw, bh, default, refused_by) in libaom {
        match filter_intra_size_class_rect(bw, bh) {
            Some(class) => {
                assert!(
                    refused_by.is_none(),
                    "{bw}x{bh} has a CDF class but is listed as refused by {refused_by:?} -- \
                     one of the two is stale"
                );
                assert_eq!(
                    crate::cdf::FILTER_INTRA[class][0],
                    default,
                    "{bw}x{bh} (class {class}) must carry default_filter_intra_cdfs' own row"
                );
                assert_eq!(
                    &crate::cdf::FILTER_INTRA[class][1..],
                    &[32768u16, 0][..],
                    "{bw}x{bh} (class {class}) is a 2-symbol CDF: {{p, 32768, count}}"
                );
                assert!(
                    classes_seen.insert(class),
                    "class {class} is claimed by two shapes -- one of them reads the other's \
                     probability"
                );
            }
            None => {
                let why = refused_by.unwrap_or_else(|| {
                    panic!(
                        "{bw}x{bh} is inside av1_filter_intra_allowed_bsize and reachable, \
                         but has no CDF class -- its use_filter_intra symbol is never read"
                    )
                });
                assert!(
                    !why.is_empty(),
                    "{bw}x{bh}'s refusal string must name the blocker"
                );
            }
        }
    }
    // The other direction: every row of `cdf::FILTER_INTRA` was claimed
    // by exactly one shape above, so no row can be left over (or shared)
    // once a class is added or removed.
    assert_eq!(
        classes_seen.len(),
        crate::cdf::FILTER_INTRA.len(),
        "cdf::FILTER_INTRA has {} rows but only {:?} are claimed by an allowed shape",
        crate::cdf::FILTER_INTRA.len(),
        classes_seen
    );
    // Past the bound on either axis: no symbol at all (spec
    // `filter_intra_mode_info` never reads one).
    for (bw, bh) in [(64, 64), (64, 32), (32, 64)] {
        assert!(
            filter_intra_size_class_rect(bw, bh).is_none(),
            "{bw}x{bh} is past av1_filter_intra_allowed_bsize's <=32 bound"
        );
    }
}
use super::*;

/// lane-tx64x16 r3: the two new 4:1 chroma scans are a TRANSPOSED PAIR --
/// `SCAN_4X16[i]` must name the transpose of the position
/// `SCAN_16X4[i]` names (class scan-weights-cross-axis: a naive copy of
/// libaom's column-major `default_scan_*` installs the transpose and only
/// an asymmetric check catches it). Also pins both as permutations, and
/// pins the same relation for the 32x8/8x32 luma pair this arm reuses.
#[test]
fn the_rect4_scans_are_transposed_pairs() {
    for (a, b, w, h) in [
        (&SCAN_16X4[..], &SCAN_4X16[..], 16usize, 4usize),
        (&SCAN_32X8[..], &SCAN_8X32[..], 32, 8),
    ] {
        let mut seen_a: Vec<u16> = a.to_vec();
        seen_a.sort_unstable();
        assert_eq!(
            seen_a,
            (0..(w * h) as u16).collect::<Vec<_>>(),
            "{w}x{h} scan is not a permutation of its positions"
        );
        for (i, (&pa, &pb)) in a.iter().zip(b.iter()).enumerate() {
            let (row, col) = (pa as usize / w, pa as usize % w);
            assert_eq!(
                pb as usize,
                col * w.min(h) + row,
                "scan step {i}: {w}x{h} visits (row {row}, col {col}) but its transpose \
                 visits position {pb}"
            );
        }
    }
}

/// lane-coef2 (class `enumerate-table-domain`): the padded level buffers
/// [`read_coeffs`]/[`read_coeffs_rect`] hand to [`neighbour`] dropped the
/// `row >= side || col >= side` edge test, so the padding must cover the
/// WIDEST offset any `base_ctx` class reads -- `(row + 4, col)` and
/// `(row, col + 4)` -- at every transform size this decoder codes, and
/// the buffer must be no larger than the fixed frames those two functions
/// declare (`36 * 36` square, `768` rect).
#[test]
fn padded_level_buffers_cover_every_neighbour_read_at_every_transform_size() {
    for side in [4usize, 8, 16, 32] {
        let stride = side + 4;
        let (row, col) = (side - 1, side - 1);
        let widest = ((row + 4) * stride + col + 4).max(row * stride + col + 4);
        assert!(widest < stride * stride, "square {side}: {widest}");
        assert!(stride * stride <= 36 * 36, "square {side} frame");
    }
    for (w, h) in [
        (4usize, 8usize),
        (8, 4),
        (8, 16),
        (16, 8),
        (4, 16),
        (16, 4),
        (8, 32),
        (32, 8),
        (16, 32),
        (32, 16),
    ] {
        let stride = w + 4;
        let n = stride * (h + 4);
        let (row, col) = (h - 1, w - 1);
        let widest = ((row + 4) * stride + col + 4).max(row * stride + col + 4);
        assert!(widest < n, "rect {w}x{h}: {widest} >= {n}");
        assert!(n <= 768, "rect {w}x{h} frame {n}");
        assert!(w.is_power_of_two(), "rect {w}x{h} stride mask");
    }
}

/// lane-rectsplit r4 (class `enumerate-table-domain`): [`base_ctx`]'s
/// `TX_CLASS_2D` arm must, over the WHOLE 5x5 domain of both rect shapes
/// and the square one, return exactly what libaom's
/// `av1_nz_map_ctx_offset[tx_size][coeff_idx]` holds -- the flat tables
/// transcribed in [`cdf`], read at libaom's own COLUMN-major
/// `coeff_idx = col * 32 + row`, i.e. `table[col][row]`. r1..r3 read them
/// `[row][col]`, which is the transpose (the OTHER shape's rule, with 11
/// and 16 swapped): it desynced the first superblock strip whose luma
/// corner held a coefficient off the first row/column.
/// lane-coefread: the per-coefficient neighbour gather now reads FLAT
/// offsets ([`flat_taps`]) instead of `(row + dr, col + dc)`. Same cells,
/// every class, every square size, over the whole `(row, col)` domain.
#[test]
fn flat_taps_match_the_row_col_neighbour_reads() {
    for &side in &[4usize, 8, 16, 32] {
        let stride = side + 4;
        let mut grid = vec![0u8; stride * stride];
        let mut x = 12345u32;
        for c in grid.iter_mut() {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            *c = ((x >> 16) & 15) as u8;
        }
        for class in [TxClass::TwoD, TxClass::Horiz, TxClass::Vert] {
            let (bt, rt) = flat_taps(stride, class);
            for row in 0..side {
                for col in 0..side {
                    let idx = row * stride + col;
                    if !(class == TxClass::TwoD && row == 0 && col == 0) {
                        let want = base_ctx(&grid, stride, row, col, class, None);
                        let got = base_mag(&grid, idx, &bt)
                            + match class {
                                TxClass::TwoD => {
                                    cdf::NZ_MAP_CTX_OFFSET_32[row.min(4)][col.min(4)] as usize
                                }
                                TxClass::Horiz => nz_map_ctx_offset_1d(col.min(31)),
                                TxClass::Vert => nz_map_ctx_offset_1d(row.min(31)),
                            };
                        assert_eq!(got, want, "base side {side} at {row},{col}");
                    }
                    let want = br_ctx(&grid, stride, row, col, class);
                    let mag = br_mag(&grid, idx, &rt);
                    let got = if row == 0 && col == 0 {
                        mag
                    } else {
                        let near = match class {
                            TxClass::TwoD => row < 2 && col < 2,
                            TxClass::Horiz => col == 0,
                            TxClass::Vert => row == 0,
                        };
                        if near { mag + 7 } else { mag + 14 }
                    };
                    assert_eq!(got, want, "br side {side} at {row},{col}");
                    assert_eq!(
                        br_ctx_rect(&grid, stride, row, col, class),
                        want,
                        "br_rect side {side} at {row},{col}"
                    );
                }
            }
        }
    }
}

#[test]
fn base_ctx_rect_offsets_match_the_transcribed_tables_over_the_whole_domain() {
    let grid = [0u8; 36 * 36];
    for (shape, table) in [
        (Some((64usize, 32usize)), &cdf::NZ_MAP_CTX_OFFSET_64X32),
        (Some((32, 64)), &cdf::NZ_MAP_CTX_OFFSET_32X64),
        (None, &cdf::NZ_MAP_CTX_OFFSET_32),
    ] {
        // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
        #[allow(clippy::needless_range_loop)]
        for row in 0..5usize {
            // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
            #[allow(clippy::needless_range_loop)]
            for col in 0..5usize {
                // An all-zero neighbourhood makes the magnitude term 0,
                // so `base_ctx` returns the offset itself.
                let got = base_ctx(&grid, 36, row, col, TxClass::TwoD, shape);
                let want = if row == 0 && col == 0 {
                    0
                } else {
                    table[col][row] as usize
                };
                assert_eq!(got, want, "{shape:?} at (row {row}, col {col})");
            }
        }
    }
}

// `crate::tile`'s `flat_key_frame_tile`/`dc_key_frame_tile_levels`/
// `split_dc_key_frame_tile` are not exercised here: they are synthetic
// single-purpose writers with their own skip-context rule (keyed to
// superblock position, not the neighbour-tracked one every real block
// uses), never reached by `encode_key_frame_with_modes` — decoding them
// would need a second, throwaway context convention next to the real
// one below. `sb_coeff_key_frame_tile`, what the real encoder writes, is
// this decoder's one target, and the round-trip test below is against
// that path with real quantised residual, not a synthetic all-DC frame.

/// [`SCAN_8X4`]/[`SCAN_4X8`] (lane-rectx r2): a bijection check alone
/// cannot catch a transposed pair -- a swap of two orientation tables is
/// still a bijection over the same 32 positions. This pins BOTH arrays
/// at once, element-for-element, against literal transcriptions of
/// libaom's real `default_scan_4x8`/`default_scan_8x4` in
/// `av1/common/scan.c` (`~/.cache/aom-oracle/src`, read directly for
/// this test, not re-derived from the zigzag formula). The two source
/// sequences are themselves different in their own right (not equal to
/// each other), so this is asymmetric in the two axes: if the repo's
/// `SCAN_8X4`/`SCAN_4X8` were swapped with each other, one side of this
/// assert pair would fail even though each individual array would still
/// pass a same-orientation bijection test.
#[test]
fn scan_8x4_and_4x8_are_not_transposed_against_each_other() {
    // libaom `default_scan_4x8` verbatim (scan.c:29-32) -- this repo
    // names it `SCAN_8X4` under the established w/h axis swap (see
    // `SCAN_8X4`'s own doc comment).
    const LIBAOM_DEFAULT_SCAN_4X8: [u16; 32] = [
        0, 8, 1, 16, 9, 2, 24, 17, 10, 3, 25, 18, 11, 4, 26, 19, 12, 5, 27, 20, 13, 6, 28, 21, 14,
        7, 29, 22, 15, 30, 23, 31,
    ];
    // libaom `default_scan_8x4` verbatim (scan.c:44-47) -- this repo's
    // `SCAN_4X8`.
    const LIBAOM_DEFAULT_SCAN_8X4: [u16; 32] = [
        0, 1, 4, 2, 5, 8, 3, 6, 9, 12, 7, 10, 13, 16, 11, 14, 17, 20, 15, 18, 21, 24, 19, 22, 25,
        28, 23, 26, 29, 27, 30, 31,
    ];
    assert_ne!(
        LIBAOM_DEFAULT_SCAN_4X8, LIBAOM_DEFAULT_SCAN_8X4,
        "the two libaom source sequences must differ, or this pin would \
         pass regardless of a swap"
    );
    assert_eq!(
        SCAN_8X4, LIBAOM_DEFAULT_SCAN_4X8,
        "SCAN_8X4 vs libaom default_scan_4x8"
    );
    assert_eq!(
        SCAN_4X8, LIBAOM_DEFAULT_SCAN_8X4,
        "SCAN_4X8 vs libaom default_scan_8x4"
    );
}

/// lane-rectx r3: `av1_filter_intra_allowed_bsize` is "both sides <= 32",
/// so a DC_PRED 16x8/8x16 strip reads a `use_filter_intra` flag exactly
/// like a square 16x16 does. [`filter_intra_size_class_rect`] returned
/// `None` at those two sizes, so the symbol was never read and the tile
/// desynced from that block on (root-caused against an instrumented
/// aomdec `EC_TRACE_COEFF` ladder on a real stream: identical `mode=0`/
/// `uv_mode=9` values, our range 34808 vs the oracle's 40668 at the very
/// next transform block).
///
/// The two rows are ASYMMETRIC (9394 for 16x8, 12551 for 8x16), so this
/// pin fails if the pair is ever transposed -- the shape the repo's
/// scan tables already carry (`SCAN_8X4` == libaom `default_scan_4x8`).
#[test]
fn a_rect_strip_below_16x16_reads_its_own_filter_intra_cdf_row() {
    // `default_filter_intra_cdfs` (entropymode.c), indexed by BLOCK_SIZES_ALL:
    // [4] = BLOCK_8X16, [5] = BLOCK_16X8, [7] = BLOCK_16X32, [8] = BLOCK_32X16.
    const LIBAOM_8X16: u16 = 12551;
    const LIBAOM_16X8: u16 = 9394;
    let c16x8 = filter_intra_size_class_rect(16, 8).expect("16x8 reads use_filter_intra");
    let c8x16 = filter_intra_size_class_rect(8, 16).expect("8x16 reads use_filter_intra");
    assert_ne!(
        c16x8, c8x16,
        "the two orientations must not share a CDF row"
    );
    assert_eq!(
        cdf::FILTER_INTRA[c16x8][0],
        LIBAOM_16X8,
        "16x8 filter_intra row"
    );
    assert_eq!(
        cdf::FILTER_INTRA[c8x16][0],
        LIBAOM_8X16,
        "8x16 filter_intra row"
    );
    // The sizes one class up keep their own distinct rows (lane-intradisp).
    assert_eq!(
        cdf::FILTER_INTRA[filter_intra_size_class_rect(32, 16).unwrap()][0],
        12756
    );
    assert_eq!(
        cdf::FILTER_INTRA[filter_intra_size_class_rect(16, 32).unwrap()][0],
        14301
    );
    // A size with a side over 32 is genuinely not allowed the flag.
    assert_eq!(filter_intra_size_class_rect(64, 32), None);
}

/// `is_uni_comp_ref` (lane-av1comp): a pair is unidirectional exactly
/// when both references sit on the same temporal side -- the three
/// `uni_comp_ref` bitstream pairs (LAST/LAST2, LAST/LAST3, LAST/GOLDEN)
/// all read `true`, a forward/backward mix (LAST/BWDREF) reads `false`,
/// and a same-side non-`uni_comp_ref` pair (BWDREF/ALTREF) still reads
/// `true` (spec `has_uni_comp_refs` is symmetric in the reference set,
/// not just the three enumerated syntax pairs).
#[test]
fn is_uni_comp_ref_matches_forward_backward_sides() {
    assert!(is_uni_comp_ref(LAST_FRAME, LAST2_FRAME));
    assert!(is_uni_comp_ref(LAST_FRAME, LAST3_FRAME));
    assert!(is_uni_comp_ref(LAST_FRAME, GOLDEN_FRAME));
    assert!(is_uni_comp_ref(BWDREF_FRAME, ALTREF_FRAME));
    assert!(!is_uni_comp_ref(LAST_FRAME, BWDREF_FRAME));
    assert!(!is_uni_comp_ref(GOLDEN_FRAME, ALTREF_FRAME));
}

/// [`Neighbours::record_compound_ctx`] stamps `above_ref1`/`left_ref1`
/// with the real second reference (lane-av1comp round 12: previously
/// always cleared to `None` by `record_inter`, so the next block's own
/// `NeighbourRef::uni` -- and `comp_reference_type_ctx` downstream of it
/// -- could never see a real compound neighbour).
/// lane-interp3 r1: a decoder must never abort the process on
/// stream-derived state. `neighbour_filter` used to feed the `[3, 3]`
/// "no filter recorded" sentinel straight into
/// `InterpFilterKind::from_switchable_symbol`, whose `panic!` ("...alphabet
/// is exactly 3 symbols") crashed on a real aomenc dual-filter + OBMC
/// stream. It now refuses by name; the real symbols still decode.
#[test]
fn an_obmc_neighbour_with_no_recorded_filter_refuses_instead_of_panicking() {
    assert!(
        neighbour_filter(None, [3, 3])
            .unwrap_err()
            .to_string()
            .contains("an OBMC neighbour whose switchable interp filter was never recorded"),
        "the [3, 3] sentinel must refuse by name, never panic"
    );
    assert!(neighbour_filter(None, [3, 0]).is_err());
    let (h, v) = neighbour_filter(None, [0, 2]).expect("real symbols decode");
    assert_eq!(
        h,
        mc::InterpFilterKind::Sharp,
        "sym[1] is the horizontal kernel"
    );
    assert_eq!(
        v,
        mc::InterpFilterKind::Regular,
        "sym[0] is the vertical kernel"
    );
    let (h, v) = neighbour_filter(Some(mc::InterpFilterKind::Smooth), [3, 3])
        .expect("a fixed-filter frame never reads a symbol");
    assert_eq!(
        (h, v),
        (mc::InterpFilterKind::Smooth, mc::InterpFilterKind::Smooth)
    );
}

/// lane-t900 r27, structural audit for "an OBMC neighbour whose switchable
/// interp filter was never recorded": the `[3, 3]` sentinel is only ever
/// PUBLISHED for a cell an OBMC blend cannot pick up.
///
/// `Neighbours::record_inter_rect_mi` is the single writer of
/// `above_filter`/`left_filter` (every other recorder routes through it),
/// so the whole domain of published values is its call sites' 7th
/// argument. Two publications are safe: an INTRA cell (`is_inter = false`,
/// skipped by `overlappable_above`/`overlappable_left`'s own `is_inter`
/// gate) and a fixed-filter frame (`resolve_interp_filter` returns the
/// sentinel when `interp_fixed` is `Some`, which `neighbour_filter`
/// answers before ever looking at the symbols). What this scans for is the
/// third case -- an INTER cell stamped with a literal sentinel, which is
/// what the refusal actually fires on. Arguments are tokenised
/// positionally, never `contains()`d.
#[test]
fn every_inter_record_publishes_an_obmc_readable_filter() {
    let src = crate::test_source::module_source("decode");
    // lane-av1refusalspan: this gate is the PROOF for the refusal below, so
    // it names it and pins its guard site. The sibling test
    // `an_obmc_neighbour_with_no_recorded_filter_refuses_instead_of_panicking`
    // carries the same string 27k lines away in this file -- exactly the
    // reach a window that is not a function boundary used to have.
    const REFUSAL: &str = "an OBMC neighbour whose switchable interp filter was never recorded";
    assert!(
        crate::refusal_inventory::pins_refusal(src, REFUSAL),
        "this gate proves {REFUSAL:?}, but that string is no longer in decode.rs"
    );
    let mut calls = 0usize;
    for (start, _) in src.match_indices("record_inter_rect_mi(") {
        // Skip the definition and the wrapper's own forwarding call.
        let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
        let head = &src[line_start..start];
        // The definition, the wrapper's own forwarding call, and this
        // test's own mentions of the name (doc comment, scan pattern,
        // assert message) are not publication sites.
        if head.contains("fn ") || head.contains("///") || head.ends_with('"') {
            continue;
        }
        let mut i = start + "record_inter_rect_mi(".len();
        let (mut depth, mut arg, mut args) = (0i32, String::new(), Vec::new());
        let bytes = src.as_bytes();
        while i < bytes.len() {
            let c = bytes[i] as char;
            match c {
                '(' | '[' => depth += 1,
                ')' | ']' if depth == 0 && c == ')' => break,
                ')' | ']' => depth -= 1,
                ',' if depth == 0 => {
                    args.push(std::mem::take(&mut arg));
                    i += 1;
                    continue;
                }
                _ => {}
            }
            arg.push(c);
            i += 1;
        }
        if !arg.trim().is_empty() {
            args.push(arg);
        }
        assert_eq!(
            args.len(),
            8,
            "record_inter_rect_mi gained an argument -- re-index this audit: {args:?}"
        );
        calls += 1;
        let (is_inter, filter) = (args[4].trim(), args[6].trim());
        if filter.starts_with('[') {
            assert_eq!(
                filter.replace(' ', ""),
                "[3,3]",
                "a literal filter other than the sentinel is published at call {calls}"
            );
            assert_eq!(
                is_inter, "false",
                "an INTER cell is stamped with the [3, 3] sentinel (call {calls}): an OBMC \
                 blend picking it up refuses by name. Publish the block's own resolved \
                 switchable symbols instead."
            );
        }
    }
    assert!(
        calls >= 6,
        "the audit found only {calls} publication sites -- it is broken, not the decoder"
    );
}

/// lane-t900 r27, negative gate for "a reference frame selected with no
/// picture at this frame's own ref_frame_idx slot for it": every
/// non-`LAST_FRAME` reference resolves through `ref_planes`, and an empty
/// slot is refused BY NAME there rather than predicting from `LAST`.
///
/// Direct-call, not a stream: with this crate's writers the case is
/// unreachable end to end. A SHOWN key frame refreshes all eight slots
/// (spec 5.9.2 forces `refresh_frame_flags`, `write_frame_header` does the
/// same) and nothing ever clears a slot, so once a stream has a key frame
/// every slot holds a picture; a stream that OPENS on an inter frame is
/// stopped one guard earlier, by `decode_stream`'s own `LAST` lookup
/// ("an inter frame with no key frame before it", gated by
/// `an_inter_frame_opening_a_stream_is_refused_by_name`). The reachable
/// premise is a HIDDEN key frame refreshing only some slots, which
/// `write_frame_header` refuses to write (`show_frame` must be set) --
/// so the stream-level witness needs a hidden-frame writer, noted as the
/// open half of this proof.
#[test]
fn a_selected_reference_with_an_empty_ref_frame_idx_slot_refuses_by_name() {
    // lane-av1refusalspan: the FULL refusal, not a prefix of it. This gate
    // is the proof for the row in `refusal_inventory`, whose anchor is the
    // whole string (it carries no `(` and no `:`), and this const is what
    // that anchor reads. Shortened to `...own ref_frame_idx slot` it
    // stopped being a pin on the guard's text.
    const REFUSAL: &str = "a reference frame selected with no picture at this frame's own \
         ref_frame_idx slot for it";
    let pic = Picture {
        width: 4,
        height: 4,
        y: vec![0u16; 16],
        u: vec![0u16; 4],
        v: vec![0u16; 4],
    };
    let mut refs: [Option<&Picture>; 8] = [None; 8];
    refs[1] = Some(&pic);
    let refpix = RefPix::ready(refs);
    for ref_frame in [
        LAST2_FRAME,
        LAST3_FRAME,
        GOLDEN_FRAME,
        BWDREF_FRAME,
        ALTREF2_FRAME,
        ALTREF_FRAME,
    ] {
        let err = match ref_planes(ref_frame, &refpix) {
            Ok(_) => panic!("ref {ref_frame}: an empty slot resolved to a picture"),
            Err(e) => e.to_string(),
        };
        assert!(
            err.contains(REFUSAL),
            "ref {ref_frame}: expected {REFUSAL:?}, got: {err}"
        );
    }
    // `LAST_FRAME` never consults the slots: `decode_stream` checked it.
    let Ok((y, _, _)) = ref_planes(LAST_FRAME, &refpix) else {
        panic!("LAST_FRAME resolves to the frame's own reference planes")
    };
    assert_eq!(y.width, 4);
}

#[test]
fn record_compound_ctx_stamps_ref1_for_the_next_block_to_read() {
    let fctx = &crate::decode::FrameCtx::new();
    let mut n = Neighbours::new(4, 4, 16, 16, fctx);
    n.record_inter((0, 0), SUB, false, true, LAST_FRAME, [3, 3], false);
    assert_eq!(n.above_ref1[0], None, "record_inter alone never sets ref1");
    n.record_compound_ctx((0, 0), SUB, ALTREF_FRAME, 0, 1);
    assert_eq!(n.above_ref1[0], Some(ALTREF_FRAME));
    assert_eq!(n.left_ref1[0], Some(ALTREF_FRAME));
    assert!(!is_uni_comp_ref(LAST_FRAME, n.above_ref1[0].unwrap()));
}

/// A below-16px leaf must clear the palette neighbour state over its own
/// cell: before lane-palette2 r8 `record_palette_y_rect`'s `w / SUB` loops
/// ran zero times for any sub-16 span, so `decode_leaf_rect` /
/// `decode_leaf8` left a previous block's palette size and colours in the
/// above/left arrays and the NEXT block read `palette_y_mode` off the
/// wrong CDF row with a stale colour cache (class
/// [[cdf-row-held-constant]]).
#[test]
fn a_sub16_leaf_after_a_palette_block_yields_palette_ctx_zero() {
    let fctx = &crate::decode::FrameCtx::new();
    let mut n = Neighbours::new(4, 4, 16, 16, fctx);
    // A real 16x16 palette block covering SUB cell (1, 1).
    n.record_palette_y_rect((4, 4), 16, 16, 2, [7, 9, 0, 0, 0, 0, 0, 0]);
    let (ctx, cache) = n.palette_ctx_and_cache((1, 1));
    assert_eq!(ctx, 2, "the palette block itself must be visible");
    assert_eq!(cache, vec![7, 9]);
    // An 8x8 non-palette leaf in that same cell.
    n.record_palette_y_rect((4, 4), 8, 8, 0, [0u16; 8]);
    let (ctx, cache) = n.palette_ctx_and_cache((1, 1));
    assert_eq!(ctx, 0, "a sub-16 non-palette leaf must clear its own cell");
    assert!(cache.is_empty(), "and its stale colour cache with it");
    // Same for the chroma half.
    n.record_palette_uv_rect((4, 4), 16, 16, 2, [1, 2, 0, 0, 0, 0, 0, 0]);
    assert_eq!(n.palette_uv_cache((1, 1)), vec![1, 2]);
    n.record_palette_uv_rect((4, 4), 8, 8, 0, [0u16; 8]);
    assert!(n.palette_uv_cache((1, 1)).is_empty());
}

/// lane-publish2 (the mode-info publish surface narrowed): two invariants
/// the byte-exact stream gate only covers indirectly -- a size-0 palette
/// stamp leaves the PREVIOUS block's colours in place (spec 5.11.46
/// `get_palette_cache` reads a neighbour's colours only where its size is
/// non-zero, so writing 16 bytes of zeroes was pure traffic), and the
/// narrowed `(u16, u8)` mi maps keep their "never written" sentinel.
#[test]
fn narrowed_publish_surface_keeps_neighbour_semantics() {
    assert_eq!(
        std::mem::size_of::<(u16, u8)>(),
        4,
        "the narrowing must actually be 4 bytes"
    );
    let fctx = &crate::decode::FrameCtx::new();
    let mut n = Neighbours::new(4, 4, 16, 16, fctx);
    n.record_palette_y_rect((4, 4), 16, 16, 2, [7, 9, 0, 0, 0, 0, 0, 0]);
    n.record_palette_y_rect((4, 4), 16, 16, 0, [0u16; 8]);
    assert_eq!(
        n.above_palette_size[4], 0,
        "the size is what a reader gates on"
    );
    assert_eq!(
        n.above_palette_colors[4],
        [7, 9, 0, 0, 0, 0, 0, 0],
        "a size-0 stamp deliberately leaves the stale colours behind"
    );
    let (ctx, cache) = n.palette_ctx_and_cache((1, 1));
    assert_eq!(ctx, 0, "and they stay invisible to every reader");
    assert!(cache.is_empty());
    assert_eq!(
        n.mode_above_mi(4, 4),
        None,
        "u16::MAX sentinel = never written"
    );
    n.record_mode_mi(3, 4, 1, 1, DC_PRED);
    assert_eq!(n.mode_above_mi(4, 4), Some(DC_PRED));
}

/// [`cdf::COMPOUND_MODE_CTX_MAP`] folded by hand against libaom's
/// `av1_mode_context_analyzer` compound branch (`mvref_common.h`):
/// `comp_ctx = compound_mode_ctx_map[refmv_ctx >> 1][min(newmv_ctx, 4)]`
/// -- a few corner and interior points, not the whole 6x6 grid.
#[test]
fn compound_mode_ctx_map_matches_libaom_corners() {
    assert_eq!(cdf::COMPOUND_MODE_CTX_MAP[0][0], 0);
    assert_eq!(cdf::COMPOUND_MODE_CTX_MAP[0][4], 1);
    assert_eq!(cdf::COMPOUND_MODE_CTX_MAP[1][0], 1);
    assert_eq!(cdf::COMPOUND_MODE_CTX_MAP[1][3], 4);
    assert_eq!(cdf::COMPOUND_MODE_CTX_MAP[2][2], 5);
    assert_eq!(cdf::COMPOUND_MODE_CTX_MAP[2][4], 7);
    // `ref_mv_ctx` up to 5 (`>> 1` = 2, the map's last row) and
    // `new_mv_ctx` up to 5 (clamped to 4, `COMP_NEWMV_CTXS - 1`) are
    // exactly the ranges `find_mv_stack_compound` produces.
    assert_eq!(cdf::COMPOUND_MODE_CTX_MAP[5 >> 1][4], 7);
}

#[test]
fn a_frame_with_no_mode_info_grid_is_refused() {
    let fctx = &crate::decode::FrameCtx::new();
    assert!(
        decode_key_frame_tile(
            &[0u8; 4],
            0,
            32,
            32,
            0,
            128,
            false,
            &CdefParams::default(),
            &LoopFilterParams::default(),
            false,
            true,
            false,
            false,
            fctx,
        )
        .is_err()
    );
}

/// Encodes and decodes one picture with `modes`, asserting the decoder's
/// planes are byte-exact against the encoder's own reconstruction.
fn round_trips(w: usize, h: usize, modes: &[u8], fctx: &crate::decode::FrameCtx) {
    use crate::encode::{Encoded, Picture as Pic};
    let mut source = vec![0u16; w * h];
    for row in 0..h {
        for col in 0..w {
            source[row * w + col] = ((row * 3 + col * 5) % 251) as u16;
        }
    }
    let picture = Pic {
        width: w,
        height: h,
        y: source,
        u: vec![128; w * h / 4],
        v: vec![128; w * h / 4],
    };
    let Encoded {
        tile,
        reconstruction,
        mi_cols,
        mi_rows,
        base_q_idx,
        tx_select,
        loop_filter,
        cdef,
        loop_restoration,
        ..
    }: Encoded =
        crate::encode::encode_key_frame_with_modes_with_ctx(&picture, 40, 0.0, modes, fctx)
            .unwrap();
    let decoded = decode_key_frame_tile_lr(
        &tile,
        mi_cols,
        mi_rows,
        base_q_idx,
        w as u32,
        h as u32,
        // lane-fintra: the sequence bit the encoder wrote this tile under
        // (stale-header class -- a hard-coded `false` here reads one symbol
        // short at the first DC_PRED block once the search offers filter intra).
        crate::encode::filter_intra_on(),
        &cdef,
        &loop_filter,
        tx_select,
        true,
        false,
        false,
        &loop_restoration,
        fctx,
    )
    .unwrap();
    assert_eq!(decoded.width, w, "{w}x{h}: decoded width");
    assert_eq!(decoded.height, h, "{w}x{h}: decoded height");
    assert_eq!(decoded.y, reconstruction.y, "{w}x{h} luma mismatch");
    assert_eq!(decoded.u, reconstruction.u, "{w}x{h} chroma U mismatch");
    assert_eq!(decoded.v, reconstruction.v, "{w}x{h} chroma V mismatch");
}

#[test]
fn real_yuv_round_trips_bit_exact_against_the_encoder_reconstruction() {
    let fctx = &crate::decode::FrameCtx::for_encoder(); // lane-b128: this test ENCODES, so its decode context starts at the superblock size this process encodes at (stale-header class, as `filter_intra_on` below)
    for (w, h) in [(128usize, 128usize), (192, 128)] {
        round_trips(w, h, &[DC_PRED as u8], fctx);
    }
}

/// Every one of the thirteen key-frame intra modes, at their base angle,
/// on real (non-flat) content: the mode search picks whichever wins each
/// block, so this exercises the directional edge/reach plumbing this
/// decoder round adds, not just `DC_PRED`.
#[test]
fn every_intra_mode_round_trips_bit_exact_against_the_encoder_reconstruction() {
    let fctx = &crate::decode::FrameCtx::for_encoder(); // lane-b128: this test ENCODES, so its decode context starts at the superblock size this process encodes at (stale-header class, as `filter_intra_on` below)
    for (w, h) in [(128usize, 128usize), (192, 128)] {
        round_trips(w, h, &crate::intra::KEY_FRAME_MODES, fctx);
    }
}

/// Sizes not a whole number of 64x64 superblocks (nor even of 32x32
/// blocks, for 854x480): the gathered-CDF partial-superblock/32x32 path
/// this decoder round adds.
#[test]
fn odd_sizes_round_trip_bit_exact_against_the_encoder_reconstruction() {
    let fctx = &crate::decode::FrameCtx::for_encoder(); // lane-b128: encodes, so the decode context takes this process's superblock size
    for (w, h) in [(854usize, 480usize), (216, 96)] {
        round_trips(w, h, &crate::intra::KEY_FRAME_MODES, fctx);
    }
}

/// mod-32==8 sizes (lane-av1-rect): the true frame edge falls inside a
/// 16x16 block itself, forcing the encoder's 8x8-leaf straddle path
/// ([`crate::tile::write_leaf8`]) regardless of the RD-cost guard, since
/// no whole 16x16/32x32 legally covers the true edge here -- this decoder
/// round's leaf8 read path (`decode_leaf8`, gathered `partition_w16` split
/// bit, `partition_w8` leaves).
#[test]
fn leaf8_straddle_sizes_round_trip_bit_exact_against_the_encoder_reconstruction() {
    let fctx = &crate::decode::FrameCtx::for_encoder(); // lane-b128: this test ENCODES, so its decode context starts at the superblock size this process encodes at (stale-header class, as `filter_intra_on` below)
    for (w, h) in [(40usize, 32usize), (640, 360)] {
        round_trips(w, h, &crate::intra::KEY_FRAME_MODES, fctx);
    }
}

/// A skipped block codes no residual syntax at all (spec 5.11.34):
/// straight prediction on every plane. The real key-frame writer never
/// emits `skip = 1` ([`crate::tile::write_intra_mode`] hardcodes it to
/// `0`), so this constructs the exact symbol sequence a writer that did
/// emit it would produce, by hand, and asserts the decoded reconstruction
/// is pure DC prediction (mid-grey, with no neighbours) rather than
/// refusing.
#[test]
fn a_skipped_block_decodes_to_pure_prediction() {
    let fctx = &crate::decode::FrameCtx::new();
    use crate::msac::SymbolEncoder;
    let mut cdfs = Cdfs::new(q_ctx_of(40));
    let mut enc = SymbolEncoder::new();
    // One whole, unsplit 64x64 superblock, coded skip = true.
    enc.symbol(PARTITION_NONE, &mut cdfs.partition_w64[0]);
    enc.symbol(1, &mut cdfs.skip[0]);
    enc.symbol(
        DC_PRED,
        &mut cdfs.kf_y_mode[INTRA_MODE_CTX[DC_PRED]][INTRA_MODE_CTX[DC_PRED]],
    );
    crate::msac::SymbolDecoder::set_symr_cdf("uv_mode");
    enc.symbol(DC_PRED, &mut cdfs.uv_mode_no_cfl[DC_PRED]);
    let data = enc.finish();
    let decoded = decode_key_frame_tile(
        &data,
        16,
        16,
        40,
        64,
        64,
        false,
        &CdefParams::default(),
        &LoopFilterParams::default(),
        false,
        true,
        false,
        false,
        fctx,
    )
    .unwrap();
    assert!(
        decoded.y.iter().all(|&s| s == 128),
        "a skipped DC_PRED block with no neighbours predicts flat mid-grey"
    );
}

/// lane-av1txbands item 3: the sub-8x8 group whose CHROMA-REFERENCE leaf
/// codes `use_intrabc` AND `skip` -- the one route that arms
/// [`INTRABC_CHROMA_TX`] and then reads no chroma coefficient at all, so
/// before this round the slot stayed armed into the NEXT block's chroma
/// read (whose scan and inverse transform it then picked). No encoder in
/// hand codes this shape -- `crate::tile::write_intra_mode` hardcodes
/// `skip = 0`, and `aomenc`'s RD search never picked it on any of this
/// lane's screen-content recipes (the census in
/// `lanes/av1txbands.report.md`) -- so the symbol sequence is
/// hand-written, exactly as [`a_skipped_block_decodes_to_pure_prediction`]
/// does for a skipped block.
///
/// An 8x8 frame descends the superblock cascade to one 8x8 group without
/// coding any partition symbol at the outer levels (the true frame edge
/// forces every split), so that group's own `partition_w8` is the first
/// symbol the tile reads.
#[test]
fn a_skipped_intrabc_chroma_reference_leaf_disarms_the_inherited_chroma_tx() {
    let fctx = &crate::decode::FrameCtx::new();
    reset_skipped_intrabc_chroma_arm_hits();
    use crate::msac::SymbolEncoder;
    let mut cdfs = Cdfs::new(q_ctx_of(40));
    let mut enc = SymbolEncoder::new();
    // The frame edge forces every split above this group, so its own
    // `partition_w8` is the FIRST symbol the tile reads.
    enc.symbol(PARTITION_SPLIT, &mut cdfs.partition_w8[0]);
    // Leaves 0..2: ordinary intra `DC_PRED`, skipped (a skipped block codes
    // no coefficient at all, which is what keeps this sequence short).
    // `get_skip_txfm_context` over the group: 0 with no skipped neighbour
    // yet, 1 once leaf 0 is to the left (leaf 1) or above (leaf 2), and 2
    // for the bottom-right leaf, which has both.
    for skip_ctx in [0usize, 1, 1] {
        enc.symbol(1, &mut cdfs.skip[skip_ctx]);
        enc.symbol(0, &mut cdfs.intrabc);
        enc.symbol(
            DC_PRED,
            &mut cdfs.kf_y_mode[INTRA_MODE_CTX[DC_PRED]][INTRA_MODE_CTX[DC_PRED]],
        );
    }
    // Leaf 3 IS the group's chroma reference: skipped AND intrabc.
    enc.symbol(1, &mut cdfs.skip[2]);
    enc.symbol(1, &mut cdfs.intrabc);
    enc.symbol(0, &mut cdfs.dv_joint);
    let data = enc.finish();
    let _ = decode_key_frame_tile(
        &data,
        2,
        2,
        40,
        8,
        8,
        false,
        &CdefParams::default(),
        &LoopFilterParams::default(),
        false,
        true,
        true,
        true,
        fctx,
    );
    assert!(
        skipped_intrabc_chroma_arm_hits() >= 1,
        "the hand-written tile never reached the skipped-intrabc chroma-reference route"
    );
    assert!(
        fctx.intrabc_chroma_tx.with(std::cell::Cell::get).is_none(),
        "the inherited chroma transform slot must be disarmed when its block's chroma \
         route ends, or the NEXT block's chroma read picks its scan and inverse transform"
    );
}

/// [`a_skipped_intrabc_chroma_reference_leaf_disarms_the_inherited_chroma_tx`]'s
/// twin for the OTHER sub-8x8 group shape: a `partition_w8` of VERT sends
/// the 8x8 to [`decode_leaf_rect8`], whose chroma reference is its second
/// (right-hand) 8x4 leaf. Same route, same disarm.
#[test]
fn a_skipped_intrabc_rect_chroma_reference_leaf_disarms_the_inherited_chroma_tx() {
    let fctx = &crate::decode::FrameCtx::new();
    reset_skipped_intrabc_chroma_arm_hits();
    use crate::msac::SymbolEncoder;
    let mut cdfs = Cdfs::new(q_ctx_of(40));
    let mut enc = SymbolEncoder::new();
    enc.symbol(PARTITION_VERT, &mut cdfs.partition_w8[0]);
    // Left leaf: skipped intra `DC_PRED`.
    enc.symbol(1, &mut cdfs.skip[0]);
    enc.symbol(0, &mut cdfs.intrabc);
    enc.symbol(
        DC_PRED,
        &mut cdfs.kf_y_mode[INTRA_MODE_CTX[DC_PRED]][INTRA_MODE_CTX[DC_PRED]],
    );
    // Right leaf IS the group's chroma reference: skipped AND intrabc.
    enc.symbol(1, &mut cdfs.skip[1]);
    enc.symbol(1, &mut cdfs.intrabc);
    enc.symbol(0, &mut cdfs.dv_joint);
    let data = enc.finish();
    let _ = decode_key_frame_tile(
        &data,
        2,
        2,
        40,
        8,
        8,
        false,
        &CdefParams::default(),
        &LoopFilterParams::default(),
        false,
        true,
        true,
        true,
        fctx,
    );
    assert!(
        skipped_intrabc_chroma_arm_hits() >= 1,
        "the hand-written tile never reached the skipped-intrabc rect chroma-reference route"
    );
    assert!(
        fctx.intrabc_chroma_tx.with(std::cell::Cell::get).is_none(),
        "the inherited chroma transform slot must be disarmed when its block's chroma \
         route ends, or the NEXT block's chroma read picks its scan and inverse transform"
    );
}

/// An inter tile's SB-level `part64` used to be read and thrown away
/// (`let _ = part64;`), recursing as SPLIT unconditionally -- a real
/// non-SPLIT value there silently desynced the whole tile instead of
/// refusing. Hand-writes a single whole superblock's `part64 = HORZ`
/// symbol (never produced by this crate's own encoder, which only ever
/// writes NONE/SPLIT at this level, but a real aomenc-class encoder can)
/// and asserts this decoder refuses by name rather than decoding garbage.
#[test]
fn a_non_split_inter_sb_partition_refuses_by_name_instead_of_desyncing() {
    let fctx = &crate::decode::FrameCtx::new();
    use crate::msac::SymbolEncoder;
    let mut cdfs = Cdfs::new(q_ctx_of(40));
    let mut enc = SymbolEncoder::new();
    enc.symbol(PARTITION_HORZ, &mut cdfs.partition_w64[0]);
    let data = enc.finish();
    let reference = crate::encode::Picture::grey(64, 64);
    let err = decode_inter_frame_tile(
        &data,
        16,
        16,
        40,
        64,
        64,
        &last_only(&reference),
        &CdefParams::default(),
        &LoopFilterParams::default(),
        false,
        false,
        Some(mc::InterpFilterKind::Regular),
        false,
        false,
        // The fixture's own implied header codes the reduced sets (the
        // wrapper's old hardcoded value).
        true,
        false,
        None,
        false,
        fctx,
    )
    .unwrap_err();
    let msg = err.to_string();
    // lane-inter4 r1: this fixture's SB value is PARTITION_HORZ, now
    // DECODED rather than refused outright; its first 64x32 strip is
    // non-skip, so the named refusal is the rectangular-inter-residual
    // one. Either string proves what the pin was written for: a non-SPLIT
    // superblock never silently decodes as SPLIT.
    // lane-r14 r2: the rectangular-residual refusal is gone (a 64x32 strip
    // now codes its truncated low-32 coefficient corner), so this
    // hand-written fixture walks further still and stops on the limit of
    // the FIXTURE rather than of the decoder: it names no reference
    // pictures. All three strings prove the one thing this pin exists for
    // -- a non-SPLIT superblock never silently decodes as SPLIT.
    assert!(
        msg.contains("SPLIT")
            || msg.contains("rectangular residual coding")
            || msg.contains("no picture at this frame's own ref_frame_idx slot"),
        "expected the named inter-SB-partition refusal, got: {msg}"
    );
}

// ffmpeg cross-oracle: an independent decoder (not this crate's own
// encoder-side reconstruction) agrees with what this module decodes,
// duplicating `crate::encode`'s own `#[cfg(test)]` helpers of the same
// name (that module is another lane's territory this round, and the
// helpers are `#[cfg(test)]`-private to it).
use crate::probe::run_with_stdin;
use std::process::{Command, Stdio};

/// Whether ffmpeg is on PATH. Absence normally SKIPs, but
/// `EC_AV1_REQUIRE_FFMPEG=1` -- or `EC_AV1_REQUIRE_AOMENC=1`, since every
/// aomenc gate decodes its stream through ffmpeg and is meaningless
/// without it -- turns it into a hard failure. Without this the require
/// flag was silently short-circuited: `!have_ffmpeg()` is evaluated first
/// in `if !have_ffmpeg() || !have_aomenc()`, so a machine with no ffmpeg
/// printed SKIP and reported green (class gate-skips-on-its-own-failure).
fn have_ffmpeg() -> bool {
    let present = Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    assert!(
        present
            || (!crate::envflags::env_flag!("EC_AV1_REQUIRE_FFMPEG")
                && !crate::envflags::env_flag!("EC_AV1_REQUIRE_AOMENC")),
        "EC_AV1_REQUIRE_FFMPEG/EC_AV1_REQUIRE_AOMENC is set but no working ffmpeg on PATH"
    );
    present
}

/// Decodes one AV1 OBU stream with ffmpeg and hands back its one 4:2:0
/// frame's planes, at ffmpeg's own (coded, block-padded) size.
fn ffmpeg_decode(stream: &[u8], width: usize, height: usize) -> Picture {
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

/// Decodes `frames` concatenated 4:2:0 frames out of one AV1 OBU stream.
fn ffmpeg_decode_sequence(
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

/// `ffprobe`'s reported coded `width,height` for one OBU stream --
/// duplicating `crate::encode`'s own helper (per-call temp naming: the
/// test binary runs callers on parallel threads, and a shared name lets
/// one test's cleanup race another's still-reading ffprobe --
/// pid-keyed-temp-path class).
fn ffprobe_size(stream: &[u8]) -> (u32, u32) {
    static PROBE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "ec-av1-decode-probe-{}-{}.obu",
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

/// This module's own decoder against an independent one (ffmpeg), not
/// just against this crate's encoder-side reconstruction: both decoders
/// read the identical tile bytes, so agreement here rules out a bug this
/// crate's own writer and reader could otherwise share.
#[test]
fn ffmpeg_and_this_decoder_agree_on_a_key_frame() {
    let fctx = &crate::decode::FrameCtx::for_encoder(); // lane-b128: this test ENCODES, so its decode context starts at the superblock size this process encodes at (stale-header class, as `filter_intra_on` below)
    if !have_ffmpeg() {
        eprintln!("SKIP ffmpeg_and_this_decoder_agree_on_a_key_frame: no ffmpeg");
        return;
    }
    use crate::encode::encode_key_frame_with_ctx;
    for &(width, height) in &[(64usize, 64usize), (216, 96), (40, 32)] {
        let picture = round_trip_test_card(width, height);
        let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
        let (coded_w, coded_h) = ffprobe_size(&encoded.stream);
        let ffmpeg_decoded = ffmpeg_decode(&encoded.stream, coded_w as usize, coded_h as usize);
        let ours = decode_key_frame_tile_lr(
            &encoded.tile,
            encoded.mi_cols,
            encoded.mi_rows,
            encoded.base_q_idx,
            coded_w,
            coded_h,
            // lane-fintra: the sequence bit the encoder wrote this tile under
            // (stale-header class -- a hard-coded `false` here reads one symbol
            // short at the first DC_PRED block once the search offers filter intra).
            crate::encode::filter_intra_on(),
            &encoded.cdef,
            &encoded.loop_filter,
            encoded.tx_select,
            true,
            encoded.screen,
            encoded.allow_intrabc,
            &encoded.loop_restoration,
            fctx,
        )
        .unwrap();
        assert_eq!(ours.y, ffmpeg_decoded.y, "{width}x{height}: luma vs ffmpeg");
        assert_eq!(ours.u, ffmpeg_decoded.u, "{width}x{height}: U vs ffmpeg");
        assert_eq!(ours.v, ffmpeg_decoded.v, "{width}x{height}: V vs ffmpeg");
    }
}

/// lane-fintra: the encoder's own filter-intra blocks, read back by BOTH
/// decoders. Two claims in one gate: the stream really exercises the tool
/// (the decoder's own `use_filter_intra == 1` counter moves, class
/// `gate-blind-to-feature` -- a feature that never fires is not measured),
/// and every sample of the frame that carries them matches ffmpeg's
/// (class `fixture-proves-symbol-not-signal`: the symbol alone would not
/// prove the recursive predictor, the tx_type row or the syntax position
/// between the palette colours and the colour-index maps).
#[test]
fn a_filter_intra_key_frame_decodes_pixel_exact_through_ffmpeg() {
    let fctx = &crate::decode::FrameCtx::for_encoder(); // lane-b128: this test ENCODES, so its decode context starts at the superblock size this process encodes at (stale-header class, as `filter_intra_on` below)
    if !crate::encode::filter_intra_on() {
        eprintln!("SKIP a_filter_intra_key_frame...: EC_AV1_FILTER_INTRA=0");
        return;
    }
    if !have_ffmpeg() {
        eprintln!("SKIP a_filter_intra_key_frame...: no ffmpeg");
        return;
    }
    let (width, height) = (128usize, 128usize);
    // Smooth, slowly curving content: what the recursive filter modes are
    // for, and what a plain directional prediction leaves residual on.
    let mut picture = crate::encode::Picture::grey(width, height);
    for row in 0..height {
        for col in 0..width {
            // lane-i64: the smooth field alone is now coded as four whole
            // 64x64 intra roots, which are too big for filter intra (the
            // tool stops at 32x32) -- so the fixture carries a hard step
            // through the middle of every superblock, which no 64x64
            // prediction fits and which sends the quadrants back to the
            // sizes the recursive modes are offered at.
            let step = if (row % 64) > 40 && (col % 64) > 24 {
                55.0
            } else {
                0.0
            };
            let v = 128.0
                + 60.0 * ((row as f64) / 9.0).sin() * ((col as f64) / 11.0).cos()
                + 8.0 * ((row + col) as f64 / 3.0).sin()
                + step;
            picture.y[row * width + col] = v.clamp(0.0, 255.0) as u16;
        }
    }
    let encoded = crate::encode::encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
    let (coded_w, coded_h) = ffprobe_size(&encoded.stream);
    let before = filter_intra_hits();
    let ours = decode_key_frame_tile_lr(
        &encoded.tile,
        encoded.mi_cols,
        encoded.mi_rows,
        encoded.base_q_idx,
        coded_w,
        coded_h,
        crate::encode::filter_intra_on(),
        &encoded.cdef,
        &encoded.loop_filter,
        encoded.tx_select,
        true,
        encoded.screen,
        encoded.allow_intrabc,
        &encoded.loop_restoration,
        fctx,
    )
    .unwrap();
    let fired = filter_intra_hits() - before;
    assert!(
        fired > 0,
        "no block took filter intra -- the gate is blind to the feature"
    );
    eprintln!("a_filter_intra_key_frame: {fired} filter-intra blocks");
    let ffmpeg_decoded = ffmpeg_decode(&encoded.stream, coded_w as usize, coded_h as usize);
    assert_eq!(ours.y, ffmpeg_decoded.y, "luma vs ffmpeg");
    assert_eq!(ours.u, ffmpeg_decoded.u, "U vs ffmpeg");
    assert_eq!(ours.v, ffmpeg_decoded.v, "V vs ffmpeg");
    assert_eq!(
        ours.y, encoded.reconstruction.y,
        "luma vs the encoder's own reconstruction"
    );
}

/// The key frame the encoder writes carries `tx_mode == TxMode::Select`,
/// and that header bit is load-bearing for a decode of its tile: read as a
/// `TxMode::Largest` frame the very first block misses its `tx_depth`
/// symbol and the whole tile desyncs. Pins the shape that broke nine gates
/// when `Encoded` had no `tx_select` for them to pass along — a test that
/// hardcoded the bit was decoding a different frame header than the one
/// the encoder wrote (and than the one `decode_stream`/ffmpeg read off the
/// wire).
#[test]
fn a_key_frames_tile_needs_the_headers_own_tx_select_bit() {
    let fctx = &crate::decode::FrameCtx::for_encoder(); // lane-b128: encodes, so the decode context takes this process's superblock size
    use crate::encode::encode_key_frame_with_ctx;
    let (width, height) = (64usize, 64usize);
    let picture = round_trip_test_card(width, height);
    let encoded = encode_key_frame_with_ctx(&picture, 100, 0.5, fctx).unwrap();
    assert!(
        encoded.tx_select,
        "the encoder's key frame no longer codes TxMode::Select -- this \
         gate's premise moved, not the decoder"
    );
    let decode = |tx_select: bool| {
        decode_key_frame_tile_lr(
            &encoded.tile,
            encoded.mi_cols,
            encoded.mi_rows,
            encoded.base_q_idx,
            width as u32,
            height as u32,
            // lane-fintra: the sequence bit the encoder wrote this tile under
            // (stale-header class -- a hard-coded `false` here reads one symbol
            // short at the first DC_PRED block once the search offers filter intra).
            crate::encode::filter_intra_on(),
            &encoded.cdef,
            &encoded.loop_filter,
            tx_select,
            true,
            encoded.screen,
            encoded.allow_intrabc,
            &encoded.loop_restoration,
            fctx,
        )
    };
    let right = decode(true).expect("the header's own tx_mode decodes");
    assert_eq!(right.y, encoded.reconstruction.y, "luma under tx_select");
    assert_eq!(right.u, encoded.reconstruction.u, "U under tx_select");
    assert_eq!(right.v, encoded.reconstruction.v, "V under tx_select");
    // The wrong bit either refuses or reconstructs something else; what it
    // must never do is agree, which would mean the bit codes nothing.
    if let Ok(wrong) = decode(false) {
        assert_ne!(
            wrong.y, encoded.reconstruction.y,
            "reading a TxMode::Select tile as Largest agreed -- the \
             tx_depth symbols are not being coded"
        );
    }
}

fn round_trip_test_card(width: usize, height: usize) -> crate::encode::Picture {
    let mut picture = crate::encode::Picture::grey(width, height);
    for row in 0..height {
        for col in 0..width {
            picture.y[row * width + col] = ((row * 7 + col * 11) % 251) as u16;
        }
    }
    for row in 0..height / 2 {
        for col in 0..width / 2 {
            let i = row * width / 2 + col;
            picture.u[i] = (100 + (col * 60 / (width / 2).max(1))) as u16;
            picture.v[i] = (200 - (row * 80 / (height / 2).max(1))) as u16;
        }
    }
    picture
}

fn panned_test_card(width: usize, height: usize, shift: i64) -> crate::encode::Picture {
    let mut picture = crate::encode::Picture::grey(width, height);
    for y in 0..height {
        for x in 0..width {
            let sx = (x as i64 - shift).rem_euclid(width as i64) as f64;
            let gradient = sx * 200.0 / width as f64;
            picture.y[y * width + x] = (20.0 + gradient).clamp(0.0, 255.0) as u16;
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

/// A GOP (one key frame plus several inter frames, each panned so its
/// blocks actually take `NEARESTMV`/`NEWMV` rather than an all-skip
/// `(0, 0)` no-op) decodes bit-exact against the encoder's own
/// reconstruction, frame by frame, chaining each decoded frame straight
/// into the next as its reference -- exactly the single-slot DPB
/// [`crate::encode::encode_sequence`] itself threads.
fn gop_round_trips(width: usize, height: usize, fctx: &crate::decode::FrameCtx) {
    use crate::encode::encode_sequence_with_ctx;
    let pictures: Vec<_> = (0..4)
        .map(|i| panned_test_card(width, height, i * 3))
        .collect();
    let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
    assert_eq!(encoded.frames.len(), 4);

    let key = &encoded.frames[0];
    let mut reference = decode_key_frame_tile_lr(
        &key.tile,
        key.mi_cols,
        key.mi_rows,
        key.base_q_idx,
        width as u32,
        height as u32,
        // lane-fintra: the sequence bit the encoder wrote this tile under
        // (stale-header class -- a hard-coded `false` here reads one symbol
        // short at the first DC_PRED block once the search offers filter intra).
        crate::encode::filter_intra_on(),
        &key.cdef,
        &key.loop_filter,
        key.tx_select,
        true,
        // This key frame's own `allow_screen_content_tools`: its intra
        // blocks carry the palette syntax under it (stale-header class).
        key.screen,
        // ... and its own `allow_intrabc`: every intra block carries a
        // `use_intrabc` symbol under it (stale-header class).
        key.allow_intrabc,
        &key.loop_restoration,
        fctx,
    )
    .unwrap();
    assert_eq!(
        reference.y, key.reconstruction.y,
        "{width}x{height} frame 0 luma"
    );
    assert_eq!(
        reference.u, key.reconstruction.u,
        "{width}x{height} frame 0 U"
    );
    assert_eq!(
        reference.v, key.reconstruction.v,
        "{width}x{height} frame 0 V"
    );

    for (i, frame) in encoded.frames.iter().enumerate().skip(1) {
        let decoded = decode_inter_frame_tile_lr(
            &frame.tile,
            frame.mi_cols,
            frame.mi_rows,
            frame.base_q_idx,
            width as u32,
            height as u32,
            &last_golden_altref(
                &reference,
                &encoded.frames[0].reconstruction,
                (i >= 2).then(|| &encoded.frames[i - 2].reconstruction),
            ),
            &frame.cdef,
            &frame.loop_filter,
            false,
            false,
            Some(mc::InterpFilterKind::Regular),
            false,
            // The encoder's own `reference_select` (spec 5.9.22): these
            // GOP round trips decode a tile the ENCODER wrote, so the
            // header bit must be the one it wrote, not a hard-coded
            // `false` (stale-header class -- a `reference_select` stream
            // reads a `comp_mode` symbol this decode would skip).
            crate::encode::reference_select(),
            // lane-intertx: this frame's own `reduced_tx_set` -- the
            // bit the chroma inheritance reduces against. The wrapper
            // used to guess `true`, which mis-derived every inherited
            // chroma type on a screen frame's widened inter blocks
            // (stale-header class).
            frame.reduced_tx_set,
            frame.tx_select,
            &frame.loop_restoration,
            Some(frame.start_cdfs.0.clone()),
            NO_SIGN_BIAS,
            // This frame's own `allow_screen_content_tools`, off the
            // `Encoded` the encoder handed back (stale-header class).
            frame.screen,
            // Same stale-header class: the bit that makes this frame's
            // inter blocks carry a `motion_mode` symbol.
            frame.switchable_motion_mode,
            // These GOP round trips decode a tile the encoder wrote, so
            // the warp bit must be the one it wrote too (lane-av1obmc2);
            // the encoder only sets it under `EC_AV1_WARP`.
            crate::encode::warp_on(),
            // lane-fintra: the sequence bit the encoder wrote this tile
            // under -- same stale-header class as the warp bit above.
            crate::encode::filter_intra_on(),
            frame.delta,
            fctx,
        )
        .unwrap();
        assert_eq!(
            decoded.y, frame.reconstruction.y,
            "{width}x{height} frame {i} luma"
        );
        assert_eq!(
            decoded.u, frame.reconstruction.u,
            "{width}x{height} frame {i} U"
        );
        assert_eq!(
            decoded.v, frame.reconstruction.v,
            "{width}x{height} frame {i} V"
        );
        reference = decoded;
    }
}

#[test]
fn a_gop_round_trips_bit_exact_against_the_encoder_reconstruction() {
    let fctx = &crate::decode::FrameCtx::for_encoder(); // lane-b128: this test ENCODES, so its decode context starts at the superblock size this process encodes at (stale-header class, as `filter_intra_on` below)
    gop_round_trips(128, 64, fctx);
}

/// Same claim, at a size that is not a whole number of 32x32 blocks
/// (216x96: 216/32 is not exact), so an inter frame's own 16x16-leaf
/// split path is exercised too.
#[test]
fn an_odd_size_gop_round_trips_bit_exact_against_the_encoder_reconstruction() {
    let fctx = &crate::decode::FrameCtx::for_encoder(); // lane-b128: this test ENCODES, so its decode context starts at the superblock size this process encodes at (stale-header class, as `filter_intra_on` below)
    gop_round_trips(216, 96, fctx);
}

/// The GOP cross-oracle: ffmpeg decodes the whole sequence the same way
/// this module does, frame for frame.
/// A NEARMV block (`not_new`, `not_zero`, `!nearest`) predicts from the
/// MV stack's *second* candidate (`near_mv`), not its first
/// (`nearest_mv`) -- a hand-built symbol stream drives
/// [`decode_inter_block`] directly, since this crate's own encoder never
/// writes NEARMV (round 3: it never chooses the mode).
#[test]
fn a_nearmv_block_predicts_from_the_stacks_second_candidate() {
    let fctx = &crate::decode::FrameCtx::new();
    use crate::msac::SymbolEncoder;
    use crate::mvstack::{MiGrid, MiInfo, find_mv_stack, single_ref_ctx};

    const LAST_FRAME: i8 = 1;
    let (mi_cols, mi_rows) = (12u32, 12u32);
    let side = 16usize; // one 16x16 leaf block
    let (r, c) = (1usize, 1usize); // `at`, in SUB (16px) units
    let (mi_row, mi_col) = (r * SUB_MI as usize, c * SUB_MI as usize);
    let bw4 = side / 4;

    let mut grid = MiGrid::new(mi_cols as usize, mi_rows as usize);
    let (above_mv, left_mv) = ((4, 4), (8, 8));
    let neighbour = |mv: (i32, i32)| MiInfo {
        is_inter: true,
        ref_frame: LAST_FRAME,
        ref_frame1: NO_REF1,
        mv1: (0, 0),
        mv: mv16(mv),
        is_new_mv: false,
        size: 1,
        size_h: 1,
        is_global_mv0: false,
        is_global_mv1: false,
    };
    for col in mi_col..mi_col + bw4 {
        grid.set(mi_row - 1, col, neighbour(above_mv));
    }
    for row in mi_row..mi_row + bw4 {
        grid.set(row, mi_col - 1, neighbour(left_mv));
    }

    let stack = find_mv_stack(
        &grid,
        mi_row,
        mi_col,
        bw4,
        bw4,
        LAST_FRAME,
        mi_cols as usize,
        mi_rows as usize,
    );
    // Sanity on the test's own setup, not the code under test: two equal-
    // weight candidates keep scan order (above, then left -- module doc).
    assert_eq!(stack.entries.len(), 2);
    assert_eq!(stack.nearest_mv, above_mv);
    assert_eq!(stack.near_mv, left_mv);

    let (skip_ctx, ii_ctx, sr_ctx) = (
        0usize,
        intra_inter_ctx(true, true, false, false),
        single_ref_ctx(false),
    );

    let mut cdfs = Cdfs::new(q_ctx_of(100));
    let mut enc = SymbolEncoder::new();
    enc.symbol(1, &mut cdfs.skip[skip_ctx]); // skip
    enc.symbol(1, &mut cdfs.intra_inter[ii_ctx]); // is_inter
    enc.symbol(0, &mut cdfs.single_ref[sr_ctx][0]); // LAST_FRAME
    enc.symbol(0, &mut cdfs.single_ref[sr_ctx][2]);
    enc.symbol(0, &mut cdfs.single_ref[sr_ctx][3]);
    enc.symbol(1, &mut cdfs.new_mv[stack.new_mv_ctx]); // not_new
    enc.symbol(1, &mut cdfs.zero_mv[stack.zero_mv_ctx]); // not_zero
    enc.symbol(1, &mut cdfs.ref_mv[stack.ref_mv_ctx]); // !nearest -> NEARMV
    let data = enc.finish();

    let (width, height) = (64usize, 64usize);
    let ref_pattern: Vec<u16> = (0..width * height).map(|i| (i % 251) as u16).collect();
    let chroma: Vec<u16> = ref_pattern
        .iter()
        .step_by(2)
        .take((width / 2) * (height / 2))
        .copied()
        .collect();
    let reference = Picture {
        width,
        height,
        y: ref_pattern.clone(),
        u: chroma.clone(),
        v: chroma,
    };
    let refpix = &last_only(&reference);
    let ref_y = whole_plane(&reference.y, width, height);

    let mut y = PlaneBuf {
        data: std::borrow::Cow::Owned(vec![0u16; width * height]),
        width,
        height,
        true_width: width,
        true_height: height,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: width,
        tile_y1: height,
        plane: 0,
    };
    let mut u = PlaneBuf {
        data: std::borrow::Cow::Owned(vec![0u16; (width / 2) * (height / 2)]),
        width: width / 2,
        height: height / 2,
        true_width: width / 2,
        true_height: height / 2,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: width / 2,
        tile_y1: height / 2,
        plane: 1,
    };
    let mut v = PlaneBuf {
        data: std::borrow::Cow::Owned(vec![0u16; (width / 2) * (height / 2)]),
        width: width / 2,
        height: height / 2,
        true_width: width / 2,
        true_height: height / 2,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: width / 2,
        tile_y1: height / 2,
        plane: 2,
    };

    let mut cdfs = Cdfs::new(q_ctx_of(100));
    let mut dec = SymbolDecoder::new(&data);
    let mut neighbours = Neighbours::new(3, 3, mi_cols as usize, mi_rows as usize, fctx);
    decode_inter_block(
        &mut dec,
        &mut cdfs,
        &mut neighbours,
        &mut grid,
        sub16_to_mi((r, c)),
        side,
        mi_cols,
        mi_rows,
        &mut y,
        &mut u,
        &mut v,
        refpix,
        &NO_SIGN_BIAS,
        &[ec_av1_syntax::WarpParams::default(); 7],
        100,
        TxbSet::Luma16,
        TxbSet::Luma16Inter,
        TxbSet::Chroma8,
        TX16,
        TX8,
        &[],
        &[],
        0,
        false,
        false,
        Some(mc::InterpFilterKind::Regular),
        false,
        None,
        false,
        false,
        false,
        false,
        0,
        0,
        [0; 7],
        false,
        [0; 2],
        false,
        false,
        false,
        side,
        side,
        false,
        (width, height),
        fctx,
    )
    .unwrap();
    // lane-wave1: a direct caller of the block decoder RECORDS its writes
    // (the tile decoder is what replays them), so this gate replays its
    // own before reading the plane back.

    let (px, py) = (c * SUB, r * SUB);
    let mut want_near = vec![0u16; side * side];
    crate::mc::predict(
        &ref_y.data,
        ref_y.width,
        ref_y.true_width,
        ref_y.true_height,
        mv_to_q4(px, left_mv.1, 0),
        mv_to_q4(py, left_mv.0, 0),
        side,
        side,
        &mut want_near,
        fctx,
    );
    let mut want_nearest = vec![0u16; side * side];
    crate::mc::predict(
        &ref_y.data,
        ref_y.width,
        ref_y.true_width,
        ref_y.true_height,
        mv_to_q4(px, above_mv.1, 0),
        mv_to_q4(py, above_mv.0, 0),
        side,
        side,
        &mut want_nearest,
        fctx,
    );
    assert_ne!(
        want_near, want_nearest,
        "test setup: the two candidate MVs must predict differently"
    );

    let mut got = vec![0u16; side * side];
    for row in 0..side {
        got[row * side..(row + 1) * side]
            .copy_from_slice(&y.data[(py + row) * y.width + px..(py + row) * y.width + px + side]);
    }
    assert_eq!(
        got, want_near,
        "NEARMV predicted from the wrong stack entry"
    );
}

#[test]
fn ffmpeg_and_this_decoder_agree_on_a_gop() {
    let fctx = &crate::decode::FrameCtx::for_encoder(); // lane-b128: this test ENCODES, so its decode context starts at the superblock size this process encodes at (stale-header class, as `filter_intra_on` below)
    if !have_ffmpeg() {
        eprintln!("SKIP ffmpeg_and_this_decoder_agree_on_a_gop: no ffmpeg");
        return;
    }
    use crate::encode::encode_sequence_with_ctx;
    let (width, height) = (128usize, 64usize);
    let pictures: Vec<_> = (0..3)
        .map(|i| panned_test_card(width, height, i * 3))
        .collect();
    let encoded = encode_sequence_with_ctx(&pictures, 100, 0.5, fctx).unwrap();
    let (coded_w, coded_h) = ffprobe_size(&encoded.stream);
    let ffmpeg_frames =
        ffmpeg_decode_sequence(&encoded.stream, coded_w as usize, coded_h as usize, 3);

    let key = &encoded.frames[0];
    let mut reference = decode_key_frame_tile_lr(
        &key.tile,
        key.mi_cols,
        key.mi_rows,
        key.base_q_idx,
        coded_w,
        coded_h,
        // lane-fintra: the sequence bit the encoder wrote this tile under
        // (stale-header class -- a hard-coded `false` here reads one symbol
        // short at the first DC_PRED block once the search offers filter intra).
        crate::encode::filter_intra_on(),
        &key.cdef,
        &key.loop_filter,
        key.tx_select,
        true,
        // This key frame's own `allow_screen_content_tools`: its intra
        // blocks carry the palette syntax under it (stale-header class).
        key.screen,
        // ... and its own `allow_intrabc`: every intra block carries a
        // `use_intrabc` symbol under it (stale-header class).
        key.allow_intrabc,
        &key.loop_restoration,
        fctx,
    )
    .unwrap();
    assert_eq!(reference.y, ffmpeg_frames[0].y, "frame 0 luma vs ffmpeg");

    for (i, frame) in encoded.frames.iter().enumerate().skip(1) {
        let decoded = decode_inter_frame_tile_lr(
            &frame.tile,
            frame.mi_cols,
            frame.mi_rows,
            frame.base_q_idx,
            coded_w,
            coded_h,
            &last_golden_altref(
                &reference,
                &encoded.frames[0].reconstruction,
                (i >= 2).then(|| &encoded.frames[i - 2].reconstruction),
            ),
            &frame.cdef,
            &frame.loop_filter,
            false,
            false,
            Some(mc::InterpFilterKind::Regular),
            false,
            // The encoder's own `reference_select` (spec 5.9.22): these
            // GOP round trips decode a tile the ENCODER wrote, so the
            // header bit must be the one it wrote, not a hard-coded
            crate::encode::reference_select(),
            // lane-intertx: the frame's own `reduced_tx_set` -- see the
            // first GOP round trip's note.
            frame.reduced_tx_set,
            frame.tx_select,
            &frame.loop_restoration,
            Some(frame.start_cdfs.0.clone()),
            NO_SIGN_BIAS,
            // This frame's own `allow_screen_content_tools`, off the
            // `Encoded` the encoder handed back (stale-header class).
            frame.screen,
            // Same stale-header class: the bit that makes this frame's
            // inter blocks carry a `motion_mode` symbol.
            frame.switchable_motion_mode,
            // These GOP round trips decode a tile the encoder wrote, so
            // the warp bit must be the one it wrote too (lane-av1obmc2);
            // the encoder only sets it under `EC_AV1_WARP`.
            crate::encode::warp_on(),
            // lane-fintra: the sequence bit the encoder wrote this tile
            // under -- same stale-header class as the warp bit above.
            crate::encode::filter_intra_on(),
            frame.delta,
            fctx,
        )
        .unwrap();
        assert_eq!(decoded.y, ffmpeg_frames[i].y, "frame {i} luma vs ffmpeg");
        assert_eq!(decoded.u, ffmpeg_frames[i].u, "frame {i} U vs ffmpeg");
        assert_eq!(decoded.v, ffmpeg_frames[i].v, "frame {i} V vs ffmpeg");
        reference = decoded;
    }
}
