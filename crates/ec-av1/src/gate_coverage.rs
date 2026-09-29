//! A guard over the real-aomenc gates' own encoder recipes.
//!
//! Every gate in [`crate::stream`] that runs a real `aomenc` picks its coding
//! tools with `--enable-*=0/1` flags, and a flag left off the command line
//! takes aomenc's default. That default is `1` for essentially every tool in
//! [`TOOL_UNIVERSE`] on a non-realtime build -- `default_extra_cfg` in
//! `av1/av1_cx_iface.c` is all-ones across the seq-level tool fields, and this
//! oracle is built with `CONFIG_REALTIME_ONLY 0`. But a `1` default is NOT
//! the same as the tool reaching the stream, and the rest of this file's
//! "defaulted means unknown" rule rests on that gap: palette and intrabc are
//! gated on CONTENT (`encoder.c:2079` derives `allow_screen_content_tools`
//! from block counts, and `seq_force_screen_content_tools` is left at the
//! adaptive `SELECT_SCREEN_CONTENT_TOOLS`), and `speed_features.c` masks
//! several tools OFF again per `--cpu-used`. A `1` default proves the encoder
//! was ALLOWED the tool, never that any block used it.
//!
//! With that default in place it is possible -- and it happened -- for a
//! coding tool to be switched *off in every single gate*, so that no real
//! stream in this repository ever exercised it and a corner-cut in the
//! decoder survived twenty gates unnoticed: `reconstruct`'s
//! `smooth_neighbor` was hardcoded `false`, wrong whenever a directional
//! block neighbours a smooth-mode one, and invisible because all twenty
//! gates passed `--enable-smooth-intra=0` (lane-chroma r1, 2026-08-30).
//!
//! The test below re-derives that set from the gate source and pins it. A tool
//! that is switched off in every gate and on in none is *provably never
//! exercised* by a real stream, and belongs on [`NEVER_EXERCISED`] with a
//! reason. Landing the decode support for one means enabling it in that
//! feature's gate, which shrinks the derived set and fails this test until the
//! list is updated -- the point being that the shrink is noticed.

// TILING is deliberately NOT in this derivation (lane-tiles r11): the check
// keys on `--enable-<tool>=0/1`, and `--tile-columns=<log2>` has no such
// shape -- a `--tile-columns=` presence check would also read `=0` (one tile)
// as coverage, which is the opposite of what it would claim. It needs no
// entry either: measured 2026-09-29 (lane-av1distwtd r2), 19 of the 146
// census-selected gate bodies spell `--tile-columns`/`--tile-rows` with a
// nonzero log2 -- 9 assert the parsed `tile_info` in the same body and 10
// more delegate to a shared helper that asserts it -- and
// `run_multi_tile_gate` covers a 2D grid with the coding tools ON at both
// bit depths.

/// aomenc coding tools that no real-`aomenc` gate is proven to exercise:
/// switched off in every gate that names them, and on in none.
///
/// A gate that simply leaves the flag off its command line does NOT close the
/// hole. aomenc's default for these is content-dependent -- palette and
/// intrabc only come on for screen content -- so "defaulted" means "unknown",
/// not "exercised", and treating it as coverage would retire the entry
/// without a single stream proving the decoder ever saw the tool. Only an
/// explicit `--enable-<tool>=1` in a gate that then asserts the feature fired
/// closes one.
///
/// Each entry is `(flag, why)`. Removing an entry is how a lane records that
/// its tool is now covered by a real stream.
#[cfg(test)]
const NEVER_EXERCISED: &[(&str, &str)] = &[
    // `enable-intrabc` LEFT this list on 2026-09-02 (lane-kf900 r6). The
    // premise it carried -- "every real stream's intrabc block sits under
    // TX_MODE_SELECT" -- was false: with `--enable-tx-size-search=0` aomenc
    // codes intrabc blocks under TX_MODE_LARGEST, and
    // `a_real_aomenc_screen_key_frame_reads_use_intrabc_on_rect_strips`
    // spells `--enable-intrabc=1` and decodes three such 8-bit streams whole-
    // frame pixel-exact against ffmpeg, with the block counter asserted > 0.
    // Re-verified 2026-09-17 (lane-av1-intrabc) at main f715d70c: 185 rect
    // `use_intrabc` symbols read, and 5 blocks took the DV path -- 4 of them
    // DECODED whole-frame pixel-exact across 4 arms (7 frames compared, 0
    // mismatches). The fifth is the testsrc2 rectangle-strip arm, which reads
    // the symbol and then refuses by name (the `intrabc_hits` bump happens on
    // the READ), so "5 blocks decoded" would overcount: 4 decoded + 1 refused,
    // exactly the single `refused == 1` the gate asserts.
];

/// The `--enable-*` tools this decoder cares about, whether or not any gate
/// names one. A flag no gate mentions is *defaulted*, i.e. unknown, and by the
/// rule above unknown is not coverage -- so the universe is fixed here rather
/// than derived from the gate source, which would silently shrink to whatever
/// the gates happen to spell.
#[cfg(test)]
const TOOL_UNIVERSE: &[&str] = &[
    "enable-1to4-partitions",
    "enable-ab-partitions",
    "enable-angle-delta",
    "enable-cdef",
    "enable-cfl-intra",
    "enable-dist-wtd-comp",
    "enable-dual-filter",
    "enable-filter-intra",
    "enable-flip-idtx",
    "enable-global-motion",
    "enable-interintra-comp",
    "enable-intra-edge-filter",
    "enable-intrabc",
    "enable-masked-comp",
    "enable-obmc",
    "enable-order-hint",
    "enable-paeth-intra",
    "enable-palette",
    "enable-rect-partitions",
    "enable-rect-tx",
    "enable-ref-frame-mvs",
    "enable-restoration",
    "enable-smooth-intra",
    "enable-superres",
    "enable-tx64",
    "enable-warped-motion",
];

/// How a non-boolean aomenc knob reads as "the tool is on".
#[cfg(test)]
#[derive(Clone, Copy, PartialEq)]
enum On {
    /// Any non-zero value enables the tool (`--tile-columns=1`, `--loopfilter-control=1`).
    NonZero,
    /// The tool's search only runs at or below this speed (`--cpu-used`).
    AtMost(u32),
}

/// Coding tools aomenc drives through a spelling [`TOOL_UNIVERSE`] does not
/// cover, listed with their aomenc default.
///
/// lane-covbd's derivation read `--enable-*` flags only, so a tool a gate pins
/// off through another spelling was invisible: every gate that named
/// `--enable-tx-size-search` passed `--enable-tx-size-search=0` when this was
/// written, and the real-stream PANIC hiding behind that pin was found by
/// lane-ab16, not by this guard. `--loopfilter-control=0`,
/// `--tile-columns=0` and a `--cpu-used` high enough to switch a search off are
/// the same shape. `enable-tx-size-search` has since been SETTLED, in both
/// directions -- see the entry below and `print_tx_size_search_census`.
///
/// The "all 49" count is GONE as of 2026-09-29 (lane-av1distwtd r2) and is
/// deliberately not replaced by a number here: it is a snapshot that rots
/// every time a recipe changes. The live count is a TEST
/// ([`print_tx_size_search_census`], below), not prose, so it cannot go stale
/// silently -- re-run it and read the numbers rather than trusting a comment.
///
/// Each entry is `(tool, spellings, aomenc default, what counts as on)`. The
/// "defaulted means unknown" rule of [`NEVER_EXERCISED`] applies unchanged: a
/// default of `1` does not prove the encoder picked the tool for any stream, so
/// only a gate that spells an on-value at that bit depth retires an entry.
// `enable-tx-size-search` SETTLED 2026-09-29 (lane-av1txsearch), by
// measurement rather than by a name-shaped guess. The r3 note here said the
// count was a FLOOR: 9 gate bodies built the flag into a `format!`
// variable that neither `flags_in` nor `settings_in` could see, because
// both scan for a literal `"--flag=value"` inside one segment. The method
// r3 asked for is now [`spelling_values`] -> [`bound_values`], and it
// resolves all of them; `print_tx_size_search_census` asserts the
// unresolvable count is ZERO, so the floor cannot come back unnoticed.
//
// THE ENTRY IS RETIRED, on an ARRIVAL ASSERT rather than on the spelling.
// `a_real_aomenc_stream_with_a_1d_tx_class_on_a_rect_transform_decodes_pixel_exact`
// spells `--enable-tx-size-search=1` as a literal and hard-asserts
// `compared_tx_depths > 0` -- "never produced a nonzero tx depth on a
// compared attempt -- the flag did not arrive". `tx_depth` is read ONLY
// under the frame header's `tx_mode_select` bit (spec 5.11.16,
// `decode.rs`: the read is guarded by `tx_select_inter`), so a nonzero
// counter IS the parsed tx-size-search behaviour, not a proxy for it.
// Measured run of that gate on this tree (aom 3.13.3): 12 pixel-exact
// decodes, 0 named refusals, 6 of the 12 at 10 BIT, 219 rect coefficient
// TUs on compared attempts, and `flag arrival: ... tx depths 53` -- so the
// tool is exercised at BOTH depths, and the assertion is not vacuous.
//
// The `tx_select_inter_gate` twins
// (`a_real_aomenc_inter_sequence_with_tx_select_decodes_pixel_exact{,_10bit}`)
// are the stronger witness still: they DELIBERATELY omit the flag
// ("DELIBERATELY ABSENT: --enable-tx-size-search=0") and assert
// `decode::txfm_split_hits()` moved, proving the var-tx `txfm_split` tree
// of spec 5.11.17 is read -- an inter transform is then a recursive split
// tree, which is the whole content of the tool.
#[cfg(test)]
const DEFAULT_ON_TOOLS: &[(&str, &[&str], &str, On)] = &[
    (
        "enable-tx-size-search",
        &["enable-tx-size-search"],
        "1",
        On::NonZero,
    ),
    (
        "enable-directional-intra",
        &["enable-directional-intra"],
        "1",
        On::NonZero,
    ),
    (
        "enable-smooth-interintra",
        &["enable-smooth-interintra"],
        "1",
        On::NonZero,
    ),
    (
        "enable-interintra-wedge",
        &["enable-interintra-wedge"],
        "1",
        On::NonZero,
    ),
    (
        "enable-diff-wtd-comp",
        &["enable-diff-wtd-comp"],
        "1",
        On::NonZero,
    ),
    (
        "enable-onesided-comp",
        &["enable-onesided-comp"],
        "1",
        On::NonZero,
    ),
    ("enable-fwd-kf", &["enable-fwd-kf"], "0", On::NonZero),
    ("deblocking", &["loopfilter-control"], "1", On::NonZero),
    (
        "multi-tile",
        &["tile-columns", "tile-rows"],
        "0",
        On::NonZero,
    ),
    ("intrabc-search", &["cpu-used"], "0", On::AtMost(2)),
];

/// Non-`--enable-*` spellings that drive a [`TOOL_UNIVERSE`] tool.
///
/// `--superres-mode=1` is how every superres gate switches superres on;
/// without this map they read as an `enable-superres` hole while a real stream
/// exercises the tool (lane-covbd deferred exactly this). The count was "all
/// three" until 2026-09-29 (lane-av1distwtd r2), which measured SIX gates
/// spelling a nonzero `--superres-mode` (5 among the 146 census-selected
/// bodies, plus `a_superres_census_over_six_real_streams_...`). No count is
/// given here for the same reason as `enable-tx-size-search` above: it rots.
/// Worth noting the alias is the ONLY positive evidence for superres, and
/// those gates do assert arrival -- `superres_hits` / `predict_scaled_hits`
/// are read in five of them -- so this one is a healthy alias, not a
/// spelling-only one.
#[cfg(test)]
const ALIASES: &[(&str, &str)] = &[("superres-mode", "enable-superres")];

/// Coverage is per (tool x bit depth), not per tool.
///
/// lane-hbdinter found two 10-bit-only defects (SGR box sums unscaled, Wiener
/// clamp pinned at the 8-bit bound) that survived the "10-bit bit-exact"
/// milestone because every 10-bit gate passed `--enable-restoration=0`:
/// `enable-restoration` is positively exercised -- at 8 bits only. A tool the
/// 8-bit gates cover says nothing about the high-bit-depth path through the
/// same code, so the two lists below are pinned separately.
///
/// Each entry is `(flag, why)`. A gate that passes `--enable-<tool>=1` at that
/// depth retires the entry, and this test fails until it is deleted.
#[cfg(test)]
const NEVER_EXERCISED_8BIT: &[(&str, &str)] = &[
    // `enable-cfl-intra` LEFT both lists on 2026-09-02 (lane-troykf r1): the
    // sb128 skipped-CfL / 1:4-chroma gate passes `--enable-cfl-intra=1` at 8
    // AND 10 bits and pixel-compares both arms.
    // `enable-dist-wtd-comp` LEFT this list on 2026-09-29 (lane-av1distwtd).
    // The entry's stated reason was WRONG: the distance-weighted compound
    // combine is not unimplemented -- `compound::dist_wtd_comp_weight_assign`
    // (lane-av1comp) and `mc::combine_compound`'s weighted path have been in
    // the tree for a long time. What was missing was a stream CARRYING the
    // tool, and `a_distance_weighted_compound_stream_decodes_pixel_exact`
    // (stream.rs) now supplies one: it spells `--enable-dist-wtd-comp=1` and
    // asserts the PARSED sequence header carries `enable_jnt_comp == true`
    // (and `false` from the same recipe with `=0`, so the bit is proven to
    // move with the flag), that `dist_wtd_comp_hits() > 0` -- the
    // `compound_idx == 0` arm, the only branch whose blend weights are not the
    // constant (8, 8) -- and pixel-compares every frame against the oracle.
    // The existing lane-cwarp compound-warp gates already SPELLED `=1` and
    // the census still read this entry as open, so the hole was a missing
    // witness, not a missing flag.
    // `enable-dual-filter` LEFT this list on 2026-09-29 (lane-av1distwtd r3),
    // paid by a DETECTOR fix rather than by a new gate. The r2 audit found
    // this entry's reason false on both halves. It is spelled -- as `=0`, by
    // `real_aomenc_1to4_streams_...` and
    // `a_real_aomenc_inter_sequence_with_32x32_level_1to4_strips_...` -- so
    // "never spelled" was wrong; and a stream IS proven to carry the tool,
    // because `a_real_aomenc_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact`
    // spells `--enable-dual-filter=1` and hard-asserts
    // `decode::dual_filter_diff_hits()` moved, the differing-direction arrival
    // assert (`resolve_interp_filter` in decode.rs). That gate builds its
    // stream through `inter_sb_none_gate`, so it spells none of the three
    // tokens `gate_bodies()` used to filter on and was INVISIBLE to the
    // census; r3 made `gate_bodies()` resolve calls as well as spellings, and
    // `the_detector_sees_the_gates_the_spelling_filter_missed` pins that.
    // Supporting measurement: with the flag left off the command line the
    // parsed sequence header already carries `enable_dual_filter = true`
    // (6855 B), while `--enable-dual-filter=0` parses false and encodes
    // different bytes (6874 B) -- so the gates pinning it `=0` were removing
    // the tool and the ones leaving it defaulted carry it.
    //
    // `enable-flip-idtx` LEFT this list the same day, also by detector fix.
    // `a_real_aomenc_stream_with_a_1d_tx_class_on_a_rect_transform_decodes_pixel_exact`
    // spells `=1` and hard-asserts `compared_class1 > 0` ("--enable-flip-idtx=1
    // never produced a V_DCT/H_DCT transform on a compared attempt"). Its
    // recipe is parameterised on `ten_bit` and names BOTH fixture formats, so
    // `is_ten_bit` filed the whole body 10-bit-ONLY and the 8-bit bucket
    // never saw it -- the lane-defon/troykf blind spot through a THIRD
    // spelling, which r3 taught `covers_both_depths` to recognise as the
    // pair-of-formats marker. Measured run of that gate: 12 pixel-exact
    // decodes, 6 of them 8-bit, 219 rect coefficient TUs of which 10 carry a
    // 1D tx class.
    // `enable-global-motion` LEFT this list on 2026-09-25 (lane-av1gwarp12 r2):
    // `a_real_affine_global_motion_stream_decodes_pixel_exact` loops
    // `for depth in [8u32, 10u32]` passing `--enable-global-motion=1` and
    // pixel-compares both depths, so the 8-bit hole has been closed since
    // lane-gmaffine r1 -- the entry survived because no suite ran a tree
    // containing both the gate and this test until the ROTZOOM suite.
    // The 12-bit ROTZOOM gate rides the high-depth bucket via `encode_12bit(`
    // in `is_ten_bit`, so it plays no part in this retirement.
    // `enable-rect-tx` -- reason RE-VERIFIED 2026-09-29 (lane-av1distwtd
    // r2), both halves still TRUE, and the second half is now PROVEN rather
    // than asserted. (1) "never spelled": measured across all 146
    // census-selected gate bodies, zero spell `--enable-rect-tx` at either
    // depth -- the only `--enable-*` tool in [`TOOL_UNIVERSE`] with no
    // spelling anywhere. (2) "reach the decoder only through partition
    // shape": aom 3.13.3 has no sequence-header bit for it. `enable_rect_tx`
    // is an `AV1E_SET_ENABLE_RECT_TX` encoder-search control
    // (`av1_cx_iface.c:2125`, copied to `txfm_cfg->enable_rect_tx` at
    // `av1_cx_iface.c:1483`) and is read ONLY inside `tx_search.c`
    // (lines 2568/2662/2686/2710/2956); `bitstream.c` writes no such bit, and
    // this crate's `SequenceHeader` (`ec-av1-syntax/src/sequence.rs`) has no
    // such field. The decoder's own gate is `is_rect_tx_allowed`, derived
    // from partition shape -- exactly what this reason says. A gate can
    // therefore never prove this tool by naming the flag, only by proving a
    // rect transform unit fired (`rect_partition_hits` /
    // `rect_coeff_tu_hits`, which lane-rect1d's gate does assert).
    // `enable-rect-tx` LEFT this list on 2026-09-29 (merge wave 2,
    // lane-av1toolgates). Both halves of the reason recorded above still hold
    // as statements about the FLAG, and the second is exactly why the closing
    // evidence is a parsed shape and not a spelling: `enable_rect_tx` has no
    // sequence-header bit, so a decoder path can never be proven by naming the
    // flag -- only by proving a rect transform unit fired.
    // `a_real_aomenc_8bit_stream_with_a_rect_transform_decodes_pixel_exact`
    // (and its 10-bit twin) now spell `--enable-rect-tx=1` in their OWN test
    // bodies -- inline on purpose, because a helper-owned recipe collapses
    // both gates into one helper segment that `is_ten_bit` files 10-bit, and
    // the 8-bit hole would stay open -- and prove the tool on the PARSED
    // shape: `rect_partition_hits()` and `rect_coeff_tu_hits()` are both
    // asserted > 0 over arms that decoded AND pixel-compared, and each
    // counter is mutation-proven red on its own message.
    // Census on the merged tree (`print_never_exercised_per_bit_depth`):
    // 255 real-aomenc gate bodies, NEVER_EXERCISED derived EMPTY at 8 bits
    // and at 10 bits -- 0 of 26 tools -- so this list is empty on the same
    // evidence, not by deletion of a claim.
];

#[cfg(test)]
const NEVER_EXERCISED_10BIT: &[(&str, &str)] = &[
    // lane-cwarp's 10-bit compound-global-warp gate closed `enable-global-motion`
    // and `enable-dist-wtd-comp` at 10 bits without deleting their entries here,
    // so this list was already stale (and this test already red) at main 9c35ecc;
    // lane-tiles r11's multi-tile 10-bit gates then closed `enable-ab-partitions`,
    // `enable-rect-partitions` and `enable-restoration` at 10 bits. All five
    // deleted together.

    // The "Only 4 of the 45 real-aomenc gates encode at 10 bits" sentence that
    // stood here is STALE (lane-av1distwtd r2, 2026-09-29), and understated
    // the high-depth bucket by more than an order of magnitude. Measured
    // against this tree with `gate_bodies()`' own filter: 146 selected gate
    // bodies, of which 83 are classified 10-bit-or-higher by `is_ten_bit`.
    // The historical point stands -- those gates pin the pixel filters and
    // the intra tool set off far more often than not -- but the counts a
    // reader would check them against no longer exist.
    // Every entry the 8-bit list carries is a hole here too. lane-hbdgates r1
    // closed six of the seven 8-bit-only entries with real 10-bit gates
    // paeth-intra, intra-edge-filter, rect-partitions, ab-partitions); its
    // seventh, the 10-bit LR gate, was `#[ignore]`d on that branch and passes
    // un-ignored on main, which carries the fix.
    // `enable-restoration` LEFT this list on 2026-09-01: lane-hbdinter's
    // 10-bit inter gate passes `--enable-restoration=1` and asserts a real
    // Wiener/SGR unit fired, which is what caught the two defects (SGR box
    // sums never brought back to the 8-bit scale, Wiener clamp at the wrong
    // bound). `enable-dist-wtd-comp` and `enable-global-motion` left it the
    // same day for the same reason: lane-cwarp's 10-bit compound global-warp
    // gate passes `=1` for both. `enable-1to4-partitions` left it on 2026-09-02:
    // lane-tx64x16 r4's 32-level 1:4 gate has a 10-bit arm that asserts both
    // orientations and coded strips inside pixel-exact attempts.
    // `enable-dual-filter` -- reason CORRECTED 2026-09-29 (lane-av1distwtd
    // r3), and this entry STAYS while its 8-bit twin was retired. That is the
    // honest outcome, not an oversight: after the r3 detector fix the 8-bit
    // bucket sees the tool exercised and the 10-bit bucket still does not.
    // The only `=1` witness is
    // `a_real_aomenc_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact`,
    // and it calls `inter_sb_none_gate(NAME, false, ...)` -- `false` is the
    // `ten_bit` parameter, so that gate builds an 8-bit stream only. Its
    // sibling `a_real_aomenc_10bit_inter_sequence_with_a_whole_superblock_block_decodes_pixel_exact`
    // passes `true`, and the 8x8-leaf pair likewise, but none of those spells
    // `--enable-dual-filter=1`. So: exercised at 8 bits (arrival assert
    // `dual_filter_diff_hits()` moved, `resolve_interp_filter` in decode.rs),
    // NOT exercised at 10 bits. Retiring this entry needs a 10-bit arm of that
    // gate -- pass `true` to `inter_sb_none_gate` and assert the differing-
    // direction counter still moves there.
    //
    // The two numbers in the `enable-flip-idtx` retirement below were
    // RE-MEASURED 2026-09-29 (lane-av1distwtd r2) by running the gate, and
    // both still hold exactly: "pixel-compares six 10-bit decodes" (the run
    // reports 12 pixel-exact decodes, 6 of them 10-bit) and "10 of its rect
    // coefficient TUs carry a 1D tx class" (219 rect coefficient TUs on
    // compared attempts, 10 of them 1D). The gate also hard-asserts
    // `compared_class1 > 0`, so its "passes =1 at both depths" is backed by an
    // arrival assert, not just a spelling.
    // `enable-dual-filter` LEFT this list on 2026-09-29 (merge wave 2,
    // lane-av1dualfilter10), by exactly the 10-bit arm the note above names.
    // `a_real_aomenc_10bit_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact`
    // spells `--enable-dual-filter=1` in its own body and hard-asserts the
    // arrival counter `decode::dual_filter_diff_hits()` moved at 10 BITS, so
    // the entry is closed at the depth it was held open at, not by a spelling
    // alone.
    // `enable-flip-idtx` LEFT this list on 2026-09-02 (lane-rect1d r1):
    // `a_real_aomenc_stream_with_a_1d_tx_class_on_a_rect_transform_decodes_pixel_exact`
    // passes `=1` at both depths and pixel-compares six 10-bit decodes, so the
    // flip/identity/1D transform types are proven to reach a real 10-bit stream
    // this decoder reconstructs exactly (10 of its rect coefficient TUs carry a
    // 1D tx class).
    // `enable-intrabc` LEFT this 10-bit list on 2026-09-02 (lane-kf900 r6).
    // The entry it first rested on was a flag spelling -- the palette gate's
    // `smptebars-screen-txs-cq40/cq55` arms pass `--enable-intrabc=1` -- and
    // lane-av1-intrabc MEASURED that those arms decode ZERO intrabc blocks at
    // both depths, so they retired the entry without a stream carrying the
    // tool (class `tool-disabled-in-every-gate`, the same defect this list
    // exists for). The closing evidence is now
    // `an_sb128_screen_stream_with_intrabc_decodes_pixel_exact`: its sb128
    // screen recipes run at 10 bits and hard-assert a decoded intrabc block
    // per depth (4-6 per arm), every frame pixel-compared against ffmpeg's own
    // 10-bit decode.
    // `enable-rect-tx` LEFT this list on 2026-09-29 (merge wave 2,
    // lane-av1toolgates), together with its 8-bit twin, by
    // `a_real_aomenc_10bit_stream_with_a_rect_transform_decodes_pixel_exact`:
    // the depth is spelled in the test's OWN body, which is what files it in
    // the 10-bit bucket, and the witness is the parsed shape
    // (`rect_partition_hits()` / `rect_coeff_tu_hits()` > 0 over arms that
    // decoded AND pixel-compared). See the 8-bit list for why the flag alone
    // could never have been the evidence.
];

/// [`DEFAULT_ON_TOOLS`] entries no 8-bit gate spells on, with the reason.
///
/// Each entry is `(tool, why)`; the reason states whether the tool is pinned
/// OFF in every gate that names it (a hard hole -- no stream can carry it) or
/// merely defaulted (unknown, and unknown is not coverage).
#[cfg(test)]
const NEVER_ON_8BIT: &[(&str, &str)] = &[];

/// [`DEFAULT_ON_TOOLS`] entries no 10-bit gate spells on, with the reason.
#[cfg(test)]
const NEVER_ON_10BIT: &[(&str, &str)] = &[];

#[cfg(test)]
mod tests {
    use super::{
        ALIASES, DEFAULT_ON_TOOLS, NEVER_EXERCISED, NEVER_EXERCISED_8BIT, NEVER_EXERCISED_10BIT,
        NEVER_ON_8BIT, NEVER_ON_10BIT, On, TOOL_UNIVERSE,
    };
    use std::collections::{BTreeMap, BTreeSet};

    /// Whether `body` contains a call to the `fn` named `name`.
    ///
    /// The name must not be the tail of a longer identifier, or
    /// `inter_sb_none_gate` would "call" `sb_none_gate`.
    fn calls_fn(body: &str, name: &str) -> bool {
        let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let mut at = 0usize;
        while let Some(rel) = body[at..].find(name) {
            let start = at + rel;
            let end = start + name.len();
            let starts_clean = start == 0 || !ident(body.as_bytes()[start - 1]);
            if starts_clean && body[end..].starts_with('(') {
                return true;
            }
            at = end;
        }
        false
    }

    /// Every `fn` defined in `stream.rs`'s test module, as `(name, body)`.
    ///
    /// The census reasons about whole `#[`-segments (see [`gate_bodies`]) but
    /// has to know which segment DEFINES or CALLS an encoder-driving `fn`, and
    /// the only way to answer that is to name the units.
    fn fn_units(src: &'static str) -> Vec<(&'static str, &'static str)> {
        const OPEN: &str = "\n    fn ";
        let mut units = Vec::new();
        let mut at = 0usize;
        while let Some(rel) = src[at..].find(OPEN) {
            let head = at + rel + OPEN.len();
            let Some(paren) = src[head..].find('(') else {
                break;
            };
            let close = src[head..]
                .find("\n    }\n")
                .map_or(src.len(), |e| head + e);
            units.push((&src[head..head + paren], &src[head..close]));
            at = close.max(head + paren + 1);
        }
        units
    }

    /// Names of every `fn` in `stream.rs` that drives the real `aomenc`,
    /// DIRECTLY (it mentions `aomenc_path()`) or by DELEGATING to one.
    ///
    /// lane-av1distwtd r3: the transitive closure is the whole point. A gate
    /// that hands its recipe to a shared helper (`inter_sb_none_gate` and
    /// friends) never spells `--passes=1` itself, so the old token filter
    /// dropped it -- 165 of 311 segments, 6 of which spell `--enable-X=1`.
    /// That is not a coverage fact, it is a segmentation fact, and it is what
    /// kept `enable-dual-filter` on the never-exercised lists while a named
    /// gate exercised the tool and hard-asserted the arrival.
    fn encoder_fns(src: &'static str) -> BTreeSet<&'static str> {
        let units = fn_units(src);
        let mut enc: BTreeSet<&'static str> = units
            .iter()
            .filter(|(_, body)| body.contains("aomenc_path()"))
            .map(|(name, _)| *name)
            .collect();
        // Fixpoint: `inter_sb_none_gate` is an encoder fn because it names
        // `aomenc_path()`; the `#[test]` that only CALLS it is a gate too.
        loop {
            let mut grew = false;
            for (name, body) in &units {
                if enc.contains(name) {
                    continue;
                }
                if enc.iter().any(|e| calls_fn(body, e)) {
                    enc.insert(name);
                    grew = true;
                }
            }
            if !grew {
                return enc;
            }
        }
    }

    /// The token spellings the filter used before it resolved calls. Kept as a
    /// floor: a gate is still recognised by these even if the call graph
    /// misses it, so the fix can only ever ADD gates, never lose one.
    fn legacy_gate_tokens(body: &str) -> bool {
        body.contains("\"--passes=1\"")
            || body.contains("ten_bit_tool_gate(")
            || body.contains("encode_10bit_gradients")
    }

    /// Bodies of the real-aomenc gates: a `fn` body that drives the real
    /// `aomenc`, however it gets there. Split on the attribute that opens a
    /// test.
    ///
    /// lane-av1distwtd r3: a segment qualifies if it spells one of the
    /// [`legacy_gate_tokens`] OR names an encoder `fn` -- either by defining
    /// one (it mentions `aomenc_path()`) or by calling one. Recognition by
    /// CALL is what a helper-delegating gate needs: its flags live in the
    /// helper it calls, and it is invisible to a spelling-only filter.
    fn gate_bodies() -> Vec<&'static str> {
        let src = include_str!("stream.rs");
        let enc = encoder_fns(src);
        let gates: Vec<&str> = src
            .split("\n    #[")
            // lane-hbdgates r1: an `#[ignore]`d gate exercises nothing, so it
            // must not close a hole. Its body is the segment that opens with
            // the ignore attribute.
            .filter(|body| !body.starts_with("ignore"))
            // lane-av1distwtd r3: a segment qualifies by what it SPELLS
            // (`legacy_gate_tokens`) or by what it CALLS. `calls_fn` is
            // deliberately the weak half of an OR, so the fix can only add
            // gates -- it can never stop recognising one the old filter did.
            .filter(|body| {
                legacy_gate_tokens(body)
                    || body.contains("aomenc_path()")
                    || enc.iter().any(|e| calls_fn(body, e))
            })
            .collect();
        assert!(
            gates.len() >= 20,
            "expected the real-aomenc gates, found {}",
            gates.len()
        );
        gates
    }

    /// `--enable-<flag>=<value>` settings spelled inside one gate body.
    fn flags_in(gate: &str) -> BTreeMap<String, char> {
        let mut here = BTreeMap::new();
        for (i, _) in gate.match_indices("\"--enable-") {
            let rest = &gate[i + 3..];
            let Some(end) = rest.find('"') else { continue };
            let Some((flag, value)) = rest[..end].split_once('=') else {
                continue;
            };
            let Some(value) = value.chars().next() else {
                continue;
            };
            here.insert(flag.to_owned(), value);
        }
        here
    }

    /// Every `--flag=value` spelled inside one gate body, values kept whole
    /// (`--cpu-used=4`, `--tile-columns=2`), not just their first character.
    fn settings_in(gate: &str) -> BTreeMap<String, String> {
        let mut here = BTreeMap::new();
        for (i, _) in gate.match_indices("\"--") {
            let rest = &gate[i + 3..];
            let Some(end) = rest.find('"') else { continue };
            let Some((flag, value)) = rest[..end].split_once('=') else {
                continue;
            };
            here.insert(flag.to_owned(), value.to_owned());
        }
        here
    }

    /// The values a `--<flag>={var}` TEMPLATE can take in one gate body, from
    /// the local binding of `var`.
    ///
    /// lane-av1txsearch: `settings_in` reads a `--flag=value` string LITERAL,
    /// so a flag a gate builds with `format!` was invisible to it -- 10 gate
    /// bodies spell `--enable-tx-size-search` only this way, and the census
    /// filed them as "defaulted", i.e. unknown in BOTH directions. The
    /// bindings that actually exist in `stream.rs` are four shapes, and each
    /// is resolved from the gate's own text rather than from the variable's
    /// name:
    ///
    /// 1. `let <var> = if <attempt> == 0 { "0" } else { "1" };` -- an
    ///    attempt-indexed arm; the QUOTED values in the initializer are
    ///    collected. Bare integers in the initializer (`attempt % 2`) are
    ///    deliberately NOT read as flag values.
    /// 2. `for <var> in [0usize, 1]` / `["0", "1"]` -- a loop over both
    ///    values; every element of the array is collected.
    /// 3. `for (<var>, <other>) in [("1", 8u8), ("0", 8u8), ...]` -- a
    ///    tuple-destructured loop; only the element at `<var>`'s POSITION is
    ///    collected from each tuple, so the arm table's bit-depth column is
    ///    not read as a flag value.
    /// 4. `u8::from(<a>.<field>)` over a struct-literal arm table whose
    ///    `field: true` / `field: false` rows are the flag values.
    ///
    /// `None` means the binding is NOT in this body -- the variable is a
    /// parameter of a shared helper and its value is bound at the CALL SITE,
    /// which lives in a different `#[`-segment. That is a real limit of a
    /// per-segment census and is reported as unknown rather than guessed:
    /// inferring the value from the variable's name is the exact failure this
    /// function exists to remove.
    fn bound_values(body: &str, var: &str) -> Option<BTreeSet<char>> {
        // Quoted single digits: `"0"`, `"1"`.
        let quoted_digits = |t: &str| -> BTreeSet<char> {
            let mut out = BTreeSet::new();
            let mut at = 0usize;
            while let Some(rel) = t[at..].find('"') {
                let start = at + rel + 1;
                let Some(end) = t[start..].find('"') else {
                    break;
                };
                let literal = &t[start..start + end];
                if literal.len() == 1 {
                    if let Some(c) = literal.chars().next() {
                        if c.is_ascii_digit() {
                            out.insert(c);
                        }
                    }
                }
                at = start + end + 1;
            }
            out
        };
        // Bare integer literals: `0usize`, `1`, `8u8`. Whole tokens only, so
        // `10u8` contributes nothing rather than a bogus `1`.
        let bare_digits = |t: &str| -> BTreeSet<char> {
            let mut out = BTreeSet::new();
            let bytes = t.as_bytes();
            let mut i = 0usize;
            while i < bytes.len() {
                if !bytes[i].is_ascii_digit() {
                    i += 1;
                    continue;
                }
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let clean_before = start == 0 || !is_ident_byte(bytes[start - 1]);
                // A typed literal's suffix (`0usize`, `8u8`, `1u32`) belongs to
                // the same token, so a run of LOWERCASE letters right after
                // the digit is a type, not a different identifier. Anything
                // else -- a multi-digit `10u8`, a camelCase `1x` -- is not a
                // flag value and is rejected.
                let rest = &t[i..];
                let suffix = rest
                    .bytes()
                    .take_while(|b| b.is_ascii_lowercase() || *b == b'_')
                    .count();
                let clean_after = i >= bytes.len()
                    || !is_ident_byte(bytes[i])
                    || (suffix > 0
                        && i + suffix < bytes.len()
                        && !is_ident_byte(bytes[i + suffix]));
                if clean_before && clean_after && i - start == 1 {
                    out.insert(bytes[start] as char);
                }
            }
            out
        };
        // Split a comma-separated list at TOP level only, so `("1", 8u8)`
        // stays one element.
        fn split_top<'a>(t: &'a str) -> Vec<&'a str> {
            let mut out = Vec::new();
            let (mut depth, mut start) = (0i32, 0usize);
            for (i, c) in t.char_indices() {
                match c {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => depth -= 1,
                    ',' if depth == 0 => {
                        out.push(t[start..i].trim());
                        start = i + 1;
                    }
                    _ => {}
                }
            }
            if start < t.len() {
                out.push(t[start..].trim());
            }
            out
        }
        // The body of the `[...]` that FOLLOWS `head` in `text`. Scoped to
        // the header rather than searching from the start of the body: a
        // gate's first `[` may be an unrelated array, and brackets that do
        // not balance across a whole gate body would leave the depth count
        // never returning to zero.
        fn loop_array_after<'a>(text: &'a str, head: &str) -> Option<&'a str> {
            let at = text.find(head)? + head.len();
            let open = at + text[at..].find('[')?;
            let mut depth = 0i32;
            for (i, c) in text[open..].char_indices() {
                match c {
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(&text[open + 1..open + i]);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        let mut found: Option<BTreeSet<char>> = None;
        let mut merge = |set: BTreeSet<char>| match &mut found {
            Some(all) => all.extend(set),
            None => found = Some(set),
        };
        // (1) `let <var> = if ... { "0" } else { "1" };` -- quoted values only.
        if let Some(at) = body.find(&format!("let {var} =")) {
            let init = &body[at..];
            let end = init.find(';').unwrap_or(init.len());
            merge(quoted_digits(&init[..end]));
        }
        // (2) `for <var> in [...]` -- every element of the array.
        if let Some(array) = loop_array_after(body, &format!("for {var} in ")) {
            merge(bare_digits(array));
            merge(quoted_digits(array));
        }
        // (3) `for (<var>, ...) in [(...), ...]` -- only `<var>`'s column.
        if let Some(at) = body.find("for (") {
            let head = &body[at..];
            if let Some(close) = head.find(')') {
                let pattern = &head["for (".len()..close];
                if let Some(pos) = pattern.split(',').position(|p| p.trim() == var) {
                    let header = format!("for ({pattern}) in ");
                    if let Some(array) = loop_array_after(body, &header) {
                        let mut set: BTreeSet<char> = BTreeSet::new();
                        for tuple in split_top(array) {
                            let Some(inner) =
                                tuple.strip_prefix('(').and_then(|t| t.strip_suffix(')'))
                            else {
                                continue;
                            };
                            let fields = split_top(inner);
                            let Some(field) = fields.get(pos) else {
                                continue;
                            };
                            set.extend(bare_digits(field));
                            set.extend(quoted_digits(field));
                        }
                        merge(set);
                    }
                }
            }
        }
        // (3b) `for (<var>, ...) in <ident>` where `<ident>` is a `let`
        // binding in this same body -- the arm table hoisted OUT of the
        // loop header, so the `for` line NAMES the array instead of
        // spelling it. Strategy (3) alone silently reads past the `for`
        // line to the next `[` in the body (an unrelated `&[..]` flag
        // list), finds no digits, and returns an empty set: the gate is
        // then filed as UNRESOLVED even though both of its arms sit
        // verbatim in the source. Measured: the 128x96 4:4:4 lossy
        // tx-size-search gate, whose `for (label, search, ...) in arms`
        // drove `print_tx_size_search_census`'s `unresolvable == 0`
        // assertion red on merge. The `=` hop matters: the binding's
        // TYPE annotation is itself bracketed
        // (`[(&str, &str, TxMode, usize, u64); 2]`), so scanning for the
        // first `[` after the name would return the type.
        // `<var>`'s column, read off the arm table a hoisted `for` line
        // NAMES rather than spells. Returns `None` unless the body really
        // has that shape, so an unrecognised loop is left to the other
        // strategies instead of being read as "no values".
        let named_column = |pos: usize| -> Option<BTreeSet<char>> {
            let at = body.find("for (")?;
            let head = &body[at..];
            let close = head.find(')')?;
            let after = head[close + 1..].trim_start().strip_prefix("in ")?;
            let name: &str = after
                .split(|c: char| !is_ident_byte(c as u8))
                .next()
                .filter(|n| !n.is_empty() && !n.starts_with(|c: char| c.is_ascii_digit()))?;
            let decl = body.find(&format!("let {name}"))?;
            let after_name = decl + "let ".len() + name.len();
            // `let arms_foo` must not answer for `let arms`.
            if body[after_name..]
                .chars()
                .next()
                .is_some_and(|c| is_ident_byte(c as u8))
            {
                return None;
            }
            // Hop the `=`: the binding's TYPE annotation is itself
            // bracketed, so the first `[` after the name is the type.
            let eq = after_name + body[after_name..].find('=')?;
            let open = eq + 1 + body[eq + 1..].find('[')?;
            let mut depth = 0i32;
            for (i, c) in body[open..].char_indices() {
                match c {
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            let mut set: BTreeSet<char> = Default::default();
                            for tuple in split_top(&body[open + 1..open + i]) {
                                let Some(inner) =
                                    tuple.strip_prefix('(').and_then(|t| t.strip_suffix(')'))
                                else {
                                    continue;
                                };
                                let fields = split_top(inner);
                                let Some(field) = fields.get(pos) else {
                                    continue;
                                };
                                set.extend(bare_digits(field));
                                set.extend(quoted_digits(field));
                            }
                            return Some(set);
                        }
                    }
                    _ => {}
                }
            }
            None
        };
        if let Some(at) = body.find("for (") {
            let head = &body[at..];
            if let Some(close) = head.find(')') {
                let pattern = &head["for (".len()..close];
                if let Some(pos) = pattern.split(',').position(|p| p.trim() == var) {
                    if let Some(set) = named_column(pos) {
                        merge(set);
                    }
                }
            }
        }
        // (4) `u8::from(<a>.<field>)` over a struct-literal arm table.
        for (i, _) in body.match_indices("u8::from(") {
            let arg = &body[i + "u8::from(".len()..];
            let Some(close) = arg.find(')') else { continue };
            let field = arg[..close].rsplit('.').next().unwrap_or_default().trim();
            if field.is_empty() || !field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            let mut set = BTreeSet::new();
            for (row, value) in [("true", '1'), ("false", '0')] {
                if body.contains(&format!("{field}: {row}")) {
                    set.insert(value);
                }
            }
            merge(set);
        }
        found
    }

    fn is_ident_byte(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_'
    }

    /// Values of one [`DEFAULT_ON_TOOLS`] spelling in one gate body: the
    /// literal `--<spelling>=<digit>` spellings plus every value a `format!`
    /// template built from a local binding resolves to.
    ///
    /// A gate that reaches BOTH values counts ON, not off: it builds a `=1`
    /// stream on some attempt, which is what the census asks for. This is the
    /// file's standing "any spelling that says on wins" rule
    /// (`--tile-columns=0 --tile-rows=1` is a multi-tile stream), now applied
    /// across a loop's arms as well as across spellings.
    fn spelling_values(gate: &str, spelling: &str) -> BTreeSet<char> {
        let mut out = BTreeSet::new();
        // A literal `"--<spelling>=<digit>"`: the needle ends at the `=`, so
        // the value starts AT the needle's length, not three characters in.
        let literal = format!("\"--{spelling}=");
        for (i, _) in gate.match_indices(&literal) {
            let rest = &gate[i + literal.len()..];
            let Some(end) = rest.find('"') else { continue };
            if let Some(c) = rest[..end].chars().next() {
                if c.is_ascii_digit() {
                    out.insert(c);
                }
            }
        }
        // A template `format!("--<spelling>={<var>}")`: the placeholder name
        // is the part between the braces, and its VALUE is whatever the
        // binding of `<var>` in this same body reaches.
        let template = format!("format!(\"--{spelling}=");
        for (i, _) in gate.match_indices(&template) {
            let rest = &gate[i + template.len()..];
            let Some(end) = rest.find('"') else { continue };
            let Some(var) = rest[..end]
                .strip_prefix('{')
                .and_then(|v| v.strip_suffix('}'))
            else {
                continue;
            };
            if var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                out.extend(bound_values(gate, var).unwrap_or_default());
            }
        }
        out
    }

    /// Whether a gate body spells one [`DEFAULT_ON_TOOLS`] entry on / off,
    /// over [`spelling_values`] -- so a `format!`-built flag is classified
    /// instead of being filed as "defaulted".
    fn resolved_on_state(gate: &str, spellings: &[&str], on: On) -> Option<bool> {
        let mut state = None;
        for spelling in spellings {
            for c in spelling_values(gate, spelling) {
                let value = c.to_digit(10).unwrap_or(0);
                let is_on = match on {
                    On::NonZero => value != 0,
                    On::AtMost(limit) => value <= limit,
                };
                state = Some(state.unwrap_or(false) || is_on);
            }
        }
        state
    }

    /// `--enable-*` settings per real-aomenc gate, derived from the gate
    /// source: `flag -> (turned off in N gates, turned on in N, defaulted in N)`.
    fn tool_settings() -> (usize, BTreeMap<String, (usize, usize, usize)>) {
        let gates = gate_bodies();

        let mut per_tool: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
        let mut names: BTreeSet<String> = BTreeSet::new();
        let mut seen: Vec<BTreeMap<String, char>> = Vec::new();
        for gate in &gates {
            let here = flags_in(gate);
            names.extend(here.keys().cloned());
            seen.push(here);
        }
        for name in names {
            let off = seen.iter().filter(|g| g.get(&name) == Some(&'0')).count();
            let on = seen.iter().filter(|g| g.get(&name) == Some(&'1')).count();
            per_tool.insert(name, (off, on, gates.len() - off - on));
        }
        (gates.len(), per_tool)
    }

    #[test]
    fn every_gate_disabling_a_tool_is_a_listed_coverage_hole() {
        let (gate_count, per_tool) = tool_settings();
        // A hole OPENS when every gate names the flag and every one of them
        // says 0 -- then no real stream can exercise the tool.
        let derived: BTreeSet<&str> = per_tool
            .iter()
            .filter(|&(_, &(off, on, defaulted))| off == gate_count && on == 0 && defaulted == 0)
            .map(|(name, _)| name.as_str())
            .collect();
        // A hole CLOSES only on positive evidence: some gate passes `=1`.
        // A gate that merely leaves the flag off its command line proves
        // nothing -- aomenc's default for palette and intrabc is content-
        // dependent -- so "defaulted" must not retire an entry, or a listing
        // would disappear the moment an unrelated gate is added.
        let exercised: BTreeSet<&str> = per_tool
            .iter()
            .filter(|&(_, &(_, on, _))| on > 0)
            .map(|(name, _)| name.as_str())
            .collect();
        let listed: BTreeSet<&str> = NEVER_EXERCISED.iter().map(|&(flag, _)| flag).collect();

        let unlisted: Vec<&&str> = derived.difference(&listed).collect();
        assert!(
            unlisted.is_empty(),
            "these aomenc tools are switched off in all {gate_count} real-aomenc gates and on in \
             none, so no real stream exercises them, and they are not listed in NEVER_EXERCISED: \
             {unlisted:?} -- either enable the tool with `=1` in its own gate or add it to the \
             list with a reason"
        );
        let stale: Vec<&&str> = listed.intersection(&exercised).collect();
        assert!(
            stale.is_empty(),
            "NEVER_EXERCISED lists {stale:?}, but a gate now passes `=1` for them -- delete \
             those entries, the coverage hole is closed"
        );
    }

    /// The derivation is only meaningful if a flag left off a gate's command
    /// line really does mean "aomenc's default", i.e. the parser above is
    /// reading whole flags and not fragments.
    #[test]
    fn tool_settings_reads_whole_flags() {
        let (gate_count, per_tool) = tool_settings();
        assert!(
            per_tool.contains_key("enable-cdef"),
            "expected --enable-cdef among the gate flags"
        );
        for (name, &(off, on, defaulted)) in &per_tool {
            assert!(
                !name.is_empty() && !name.contains(['"', '=']),
                "malformed flag {name:?}"
            );
            assert_eq!(
                off + on + defaulted,
                gate_count,
                "{name} counts do not cover every gate"
            );
        }
    }

    /// A gate encodes at 10 bits when its recipe says so: the shared 10-bit
    /// helper, an explicit `--bit-depth=10`/`--input-bit-depth=10`, or a
    /// `yuv420p10le` fixture handed to aomenc. Everything else is aomenc's
    /// 8-bit default.
    /// lane-defon r1: a gate helper parameterised on `bit_depth` builds BOTH
    /// an 8-bit and a 10-bit stream from one recipe, so its flags cover both
    /// depths -- classifying it by the 10-bit strings inside its conditional
    /// alone hid `--enable-tx-size-search=1` from the 8-bit list.
    /// lane-troykf r1: the same blind spot with the other spelling -- a gate
    /// that loops `for depth in [8usize, 10]` builds BOTH streams from one
    /// recipe, but its `yuv420p10le` arm made `is_ten_bit` classify it as
    /// 10-bit only, hiding `--enable-cfl-intra=1` from the 8-bit list.
    /// lane-av1-intrabc r1: the shared recipe helper
    /// `screen_intrabc_stream_at_depth` is parameterised on `depth` and sits in
    /// the segment that opens at the PRECEDING gate's `#[test]`, so without
    /// this marker that gate's own 8-bit-only flags read as 10-bit-only -- the
    /// lane-defon/troykf blind spot with `depth` spelled instead of
    /// `bit_depth`. Three gates share the helper, at both depths.
    /// lane-av1distwtd r3: the THIRD spelling of the same blind spot, still
    /// live after lane-defon and lane-troykf. A recipe parameterised on
    /// `ten_bit` names BOTH fixture formats in one body -- `let pix_fmt = if
    /// ten_bit { "yuv420p10le" } else { "yuv420p" };` -- so `is_ten_bit` saw
    /// the 10-bit arm and filed the whole body 10-bit-ONLY, hiding its 8-bit
    /// arm from the 8-bit bucket. The marker is the PAIR OF FORMATS, not the
    /// parameter name: a body spelling both pixel formats builds streams at
    /// both depths, whatever it calls the flag. The quotes matter -- a bare
    /// `yuv420p` substring test also matches `yuv420p10le` and would fire on
    /// every 10-bit gate, retiring entries on no evidence.
    ///
    /// This is what mis-filed `a_real_aomenc_stream_with_a_1d_tx_class_on_a_rect_transform_decodes_pixel_exact`
    /// and kept `enable-flip-idtx` on the 8-bit list while that gate ran 6
    /// pixel-exact 8-bit decodes through it.
    fn covers_both_depths(body: &str) -> bool {
        body.contains("if bit_depth == 10")
            || body.contains("if depth == 10")
            || body.contains("for depth in [8usize, 10]")
            || (body.contains("\"yuv420p\"") && body.contains("\"yuv420p10le\""))
    }

    /// lane-av1distwtd r3: the detector fix, pinned. A detector change with
    /// no test can only be shown to find MORE, never to stop missing, so this
    /// asserts the three gates the two blind spots actually hid are now seen
    /// -- the two `enable-dual-filter` witnesses and the `enable-flip-idtx`
    /// both-depths gate -- and that the fix is strictly ADDITIVE against the
    /// old spelling-only filter.
    #[test]
    fn the_detector_sees_the_gates_the_spelling_filter_missed() {
        let src = include_str!("stream.rs");
        let gates = gate_bodies();
        // The helper-delegation half: each of these builds its stream through
        // `inter_sb_none_gate` and spells none of `legacy_gate_tokens`, so the
        // old filter dropped both while each hard-asserts its tool arrived.
        for name in [
            "a_real_aomenc_dual_filter_obmc_8x8_inter_sequence_decodes_pixel_exact",
            "a_real_obmc_stream_reads_a_recorded_switchable_filter_for_every_neighbour",
        ] {
            let body = gates
                .iter()
                .find(|b| b.contains(name))
                .unwrap_or_else(|| panic!("{name}: not seen by gate_bodies()"));
            assert_eq!(
                flags_in(body).get("enable-dual-filter"),
                Some(&'1'),
                "{name}: seen, but its --enable-dual-filter=1 spelling is not visible to \
                 flags_in, so recognising the gate would not retire the entry"
            );
        }
        // The both-depths half: this gate's recipe is parameterised on `ten_bit`
        // and names both fixture formats, so `covers_both_depths` must credit
        // it to the 8-bit bucket, not just the 10-bit one.
        let flip = gates
            .iter()
            .find(|b| b.contains("a_real_aomenc_stream_with_a_1d_tx_class_on_a_rect_transform"))
            .expect("the flip-idtx gate must be seen");
        assert!(
            covers_both_depths(flip),
            "the flip-idtx gate spells both fixture formats, so covers_both_depths must \
             credit it to the 8-bit bucket as well"
        );
        assert!(
            covers_depth(flip, false) && covers_depth(flip, true),
            "the flip-idtx gate must cover BOTH depths"
        );
        // Strictly additive: every segment the old spelling-only filter kept
        // is still kept, so the fix cannot have cost the census a gate.
        let legacy_only: Vec<&str> = src
            .split("\n    #[")
            .filter(|b| !b.starts_with("ignore") && legacy_gate_tokens(b))
            .collect();
        assert!(
            legacy_only.len() < gates.len(),
            "the call-resolving filter found {} gates, the old spelling-only filter found \
             {} -- the fix is expected to ADD gates, never to drop one",
            gates.len(),
            legacy_only.len()
        );
        for body in &legacy_only {
            assert!(
                gates.contains(body),
                "a gate the spelling-only filter recognised is no longer recognised: \
                 {:?}",
                &body[..body.len().min(60)]
            );
        }
    }

    /// Whether a gate body drives a stream at this depth.
    fn covers_depth(body: &str, ten_bit: bool) -> bool {
        covers_both_depths(body) || is_ten_bit(body) == ten_bit
    }

    fn is_ten_bit(body: &str) -> bool {
        // The lists pin two buckets, 8-bit and high-bit-depth; 12-bit gates
        // count in the high-depth bucket (lane-av1gwarp12's ROTZOOM gate was
        // the first 12-bit gate to positively enable a listed tool, and the
        // 10-bit-only spellings mis-filed it as 8-bit).
        body.contains("encode_10bit_gradients")
            || body.contains("ten_bit_tool_gate(")
            || body.contains("--bit-depth=10")
            || body.contains("--input-bit-depth=10")
            || body.contains("yuv420p10le")
            || body.contains("encode_12bit(")
            || body.contains("--bit-depth=12")
            || body.contains("--input-bit-depth=12")
            || body.contains("yuv420p12le")
    }

    /// Flags a gate positively enables (`=1`), at gates of the given depth.
    fn enabled_at(ten_bit: bool) -> BTreeSet<String> {
        let mut on = BTreeSet::new();
        for gate in gate_bodies() {
            if !covers_depth(gate, ten_bit) {
                continue;
            }
            for (flag, value) in flags_in(gate) {
                if value == '1' {
                    on.insert(flag);
                }
            }
            // A tool driven by its own spelling counts as exercised.
            let here = settings_in(gate);
            for (alias, tool) in ALIASES {
                if here.get(*alias).is_some_and(|v| v != "0") {
                    on.insert((*tool).to_owned());
                }
            }
        }
        on
    }

    /// The tools of [`TOOL_UNIVERSE`] that no gate of this depth enables.
    fn never_exercised_at(ten_bit: bool) -> BTreeSet<&'static str> {
        let on = enabled_at(ten_bit);
        TOOL_UNIVERSE
            .iter()
            .copied()
            .filter(|t| !on.contains(*t))
            .collect()
    }

    fn check_depth(ten_bit: bool, listed: &[(&str, &str)]) {
        let derived = never_exercised_at(ten_bit);
        let listed: BTreeSet<&str> = listed.iter().map(|&(flag, _)| flag).collect();
        let depth = if ten_bit { "10-bit" } else { "8-bit" };
        let unlisted: Vec<&&str> = derived.difference(&listed).collect();
        assert!(
            unlisted.is_empty(),
            "no {depth} gate passes `=1` for {unlisted:?}, so no real {depth} stream exercises \
             them -- enable the tool in a {depth} gate that asserts it fired, or list it"
        );
        let stale: Vec<&&str> = listed.difference(&derived).collect();
        assert!(
            stale.is_empty(),
            "the {depth} list still names {stale:?}, but a {depth} gate now passes `=1` for them \
             -- delete those entries, the coverage hole is closed"
        );
    }

    #[test]
    fn never_exercised_8bit_matches_the_gate_recipes() {
        check_depth(false, NEVER_EXERCISED_8BIT);
    }

    #[test]
    fn never_exercised_10bit_matches_the_gate_recipes() {
        check_depth(true, NEVER_EXERCISED_10BIT);
    }

    /// `cargo test -p ec-av1 --lib gate_coverage -- --nocapture` prints both
    /// lists, so the per-depth holes can be read without opening this file.
    #[test]
    fn print_never_exercised_per_bit_depth() {
        let ten: Vec<&str> = gate_bodies()
            .into_iter()
            .filter(|b| is_ten_bit(b))
            .collect();
        let total = gate_bodies().len();
        println!(
            "gate_coverage: {} real-aomenc gates, {} of them 10-bit",
            total,
            ten.len()
        );
        for (label, ten_bit) in [("8BIT", false), ("10BIT", true)] {
            let holes = never_exercised_at(ten_bit);
            println!(
                "NEVER_EXERCISED_{label} ({} of {}):",
                holes.len(),
                TOOL_UNIVERSE.len()
            );
            for flag in &holes {
                println!("    --{flag}");
            }
        }
    }

    /// [`DEFAULT_ON_TOOLS`] per depth: `tool -> (off in N gates, on in N, defaulted in N)`.
    fn default_on_settings(
        ten_bit: bool,
    ) -> (usize, BTreeMap<&'static str, (usize, usize, usize)>) {
        let gates: Vec<&str> = gate_bodies()
            .into_iter()
            .filter(|b| covers_depth(b, ten_bit))
            .collect();
        let mut per_tool = BTreeMap::new();
        for &(tool, spellings, _, on) in DEFAULT_ON_TOOLS {
            // lane-av1txsearch: `resolved_on_state`, not `default_on_state` --
            // the literal-only reader filed every `format!`-built flag as
            // "defaulted", which is unknown rather than measured.
            let states: Vec<Option<bool>> = gates
                .iter()
                .map(|g| resolved_on_state(g, spellings, on))
                .collect();
            let turned_on = states.iter().filter(|s| **s == Some(true)).count();
            let turned_off = states.iter().filter(|s| **s == Some(false)).count();
            per_tool.insert(
                tool,
                (turned_off, turned_on, gates.len() - turned_off - turned_on),
            );
        }
        (gates.len(), per_tool)
    }

    /// The [`DEFAULT_ON_TOOLS`] no gate of this depth positively enables.
    fn never_on_at(ten_bit: bool) -> BTreeSet<&'static str> {
        default_on_settings(ten_bit)
            .1
            .into_iter()
            .filter(|&(_, (_, on, _))| on == 0)
            .map(|(tool, _)| tool)
            .collect()
    }

    fn check_default_on(ten_bit: bool, listed: &[(&str, &str)]) {
        let derived = never_on_at(ten_bit);
        let listed: BTreeSet<&str> = listed.iter().map(|&(tool, _)| tool).collect();
        let depth = if ten_bit { "10-bit" } else { "8-bit" };
        let unlisted: Vec<&&str> = derived.difference(&listed).collect();
        assert!(
            unlisted.is_empty(),
            "no {depth} gate spells an on-value for {unlisted:?}, so no real {depth} stream is \
             proven to carry them -- switch the tool on in a {depth} gate that asserts it fired, \
             or list it with a reason"
        );
        let stale: Vec<&&str> = listed.difference(&derived).collect();
        assert!(
            stale.is_empty(),
            "the {depth} default-on list still names {stale:?}, but a {depth} gate now switches \
             them on -- delete those entries, the coverage hole is closed"
        );
    }

    #[test]
    fn never_on_8bit_matches_the_gate_recipes() {
        check_default_on(false, NEVER_ON_8BIT);
    }

    #[test]
    fn never_on_10bit_matches_the_gate_recipes() {
        check_default_on(true, NEVER_ON_10BIT);
    }

    /// A tool must not be pinned twice: [`TOOL_UNIVERSE`] already derives the
    /// `--enable-*` spellings under the `=1` rule.
    #[test]
    fn default_on_tools_do_not_duplicate_the_universe() {
        for &(tool, spellings, _, _) in DEFAULT_ON_TOOLS {
            assert!(
                !TOOL_UNIVERSE.contains(&tool),
                "{tool} is already covered by TOOL_UNIVERSE"
            );
            assert!(!spellings.is_empty(), "{tool} has no aomenc spelling");
        }
        for (alias, tool) in ALIASES {
            assert!(
                TOOL_UNIVERSE.contains(tool),
                "alias --{alias} names {tool}, which is not a TOOL_UNIVERSE tool"
            );
        }
    }

    /// `cargo test -p ec-av1 --lib gate_coverage -- --nocapture` prints the
    /// default-on holes with their off/defaulted split.
    #[test]
    fn print_never_on_per_bit_depth() {
        for (label, ten_bit) in [("8BIT", false), ("10BIT", true)] {
            let (gate_count, per_tool) = default_on_settings(ten_bit);
            let holes = never_on_at(ten_bit);
            println!(
                "NEVER_ON_{label} ({} of {}, over {gate_count} {label} gates):",
                holes.len(),
                DEFAULT_ON_TOOLS.len()
            );
            for tool in &holes {
                let (off, _, defaulted) = per_tool[tool];
                let spellings = DEFAULT_ON_TOOLS
                    .iter()
                    .find(|e| e.0 == *tool)
                    .map(|e| e.1.join(","))
                    .unwrap_or_default();
                println!(
                    "    {tool} (--{spellings}): off in {off}, defaulted in {defaulted}, on in 0"
                );
            }
        }
    }

    /// lane-av1txsearch: the `enable-tx-size-search` census, MEASURED rather
    /// than floored. Prints, per gate body, the values the resolver reads for
    /// the flag -- literal spellings and `format!` templates alike -- and the
    /// three-way split at the end. Re-run it after any gate recipe change:
    ///
    /// `cargo test -p ec-av1 --lib gate_coverage::tests::print_tx_size_search_census -- --nocapture`
    #[test]
    fn print_tx_size_search_census() {
        const TOOL: &str = "enable-tx-size-search";
        let (zero, one, both, unresolvable, unnamed) = {
            let (mut zero, mut one, mut both, mut unresolvable, mut unnamed) =
                (0usize, 0usize, 0usize, 0usize, 0usize);
            for gate in gate_bodies() {
                let values = spelling_values(gate, TOOL);
                let name = gate
                    .split("fn ")
                    .nth(1)
                    .and_then(|s| s.split('(').next())
                    .unwrap_or("<segment>")
                    .to_owned();
                let state = match (values.contains(&'0'), values.contains(&'1')) {
                    (false, false) => {
                        unnamed += 1;
                        "not named (defaulted)"
                    }
                    (true, false) => {
                        zero += 1;
                        "=0"
                    }
                    (false, true) => {
                        one += 1;
                        "=1"
                    }
                    (true, true) => {
                        both += 1;
                        "=0 and =1 (one arm each)"
                    }
                };
                let via = if gate.contains(&format!("format!(\"--{TOOL}=")) {
                    " [format! template resolved]"
                } else {
                    ""
                };
                let helper = if gate.contains(&format!("format!(\"--{TOOL}=")) && values.is_empty()
                {
                    " [UNRESOLVED: variable is a shared helper's parameter]"
                } else {
                    ""
                };
                if !helper.is_empty() {
                    unresolvable += 1;
                    unnamed -= 1;
                }
                println!("{name}: {state}{via}{helper}");
            }
            (zero, one, both, unresolvable, unnamed)
        };
        let total = gate_bodies().len();
        println!(
            "enable-tx-size-search over {total} census-selected gate bodies: \
             =0 only {zero}, =1 only {one}, both {both}, unresolvable {unresolvable}, \
             not named {unnamed}"
        );
        assert_eq!(
            unresolvable, 0,
            "{unresolvable} gate bodies build --{TOOL} into a variable this census cannot bind -- \
             the count is a floor again, and an unknown direction is the failure mode this file \
             exists to prevent"
        );
    }

    /// lane-av1txsearch: the RED-BEFORE proof for the template resolver, in
    /// both directions, on the gate bodies that actually exist.
    ///
    /// Direction 1 -- a `format!`-built value MOVES the classification. Each
    /// case below takes a real gate body, mutates only the bound value, and
    /// asserts the resolved state changed. A resolver that found nothing
    /// would read every one of these as "not named" and fail all four.
    ///
    /// Direction 2 -- a spelling the LITERAL reader already saw still
    /// classifies. Without this half, a resolver that overwrote the literal
    /// path would pass direction 1 while losing the 15 `=1` gates the census
    /// already counted.
    #[test]
    fn the_resolver_binds_a_format_built_flag_in_both_directions() {
        let gates = gate_bodies();
        let find = |needle: &str| -> &'static str {
            gates
                .iter()
                .copied()
                .find(|b| b.contains(needle))
                .unwrap_or_else(|| panic!("no gate body contains {needle:?}"))
        };
        // Direction 1, one per template shape.
        let cases = [
            (
                "format!(\"--enable-tx-size-search={txs}\")",
                "for txs in [0usize, 1]",
                "for txs in [0usize, 0]",
                "a_real_aomenc_palette_stream_with_8x8_leaves_decodes_pixel_exact",
            ),
            (
                "format!(\"--enable-tx-size-search={tx_search}\")",
                "if attempt % 2 == 0 { \"0\" } else { \"1\" }",
                "if attempt % 2 == 0 { \"0\" } else { \"0\" }",
                "a_real_aomenc_inter_sequence_with_a_coded_rectangular_residual_decodes_pixel_exact",
            ),
            (
                "format!(\"--enable-tx-size-search={tx_search}\")",
                "for (tx_search, bit_depth) in [(\"1\", 8u8), (\"0\", 8u8), (\"1\", 10u8)]",
                "for (tx_search, bit_depth) in [(\"0\", 8u8), (\"0\", 8u8), (\"0\", 10u8)]",
                "a_real_aomenc_stream_with_filter_intra_on_a_sub8_rect_leaf_decodes_pixel_exact",
            ),
            // The arm table hoisted OUT of the `for` line into a named
            // `let` -- the shape lane-av1lossy128x96 writes, and the one
            // that reads as UNRESOLVED unless the `for` line's collection
            // name is followed back to its binding. Without this case a
            // resolver that only understood inline arrays passes the
            // three above and still cannot bind this gate.
            (
                "format!(\"--enable-tx-size-search={search}\")",
                "\"tx size search on\",\n                \"1\",",
                "\"tx size search on\",\n                \"0\",",
                "a_real_aomenc_444_whole_64_root_tx_size_search_stream_decodes_pixel_exact_at_128x96",
            ),
        ];
        for (template, from, to, name) in cases {
            let body = find(name);
            assert!(
                body.contains(template),
                "{name} does not spell {template}, so this case proves nothing"
            );
            assert!(
                body.contains(from),
                "{name} does not contain the source binding {from:?}"
            );
            let before = spelling_values(body, "enable-tx-size-search");
            let mutated = body.replace(from, to);
            assert_ne!(
                mutated, body,
                "the mutation {from:?} -> {to:?} changed nothing"
            );
            let after = spelling_values(&mutated, "enable-tx-size-search");
            assert_eq!(
                before,
                BTreeSet::from(['0', '1']),
                "{name}: the unmutated body should resolve to both values, got {before:?}"
            );
            assert_eq!(
                after,
                BTreeSet::from(['0']),
                "{name}: mutating the bound value {from:?} -> {to:?} did not move the \
                 classification (still {after:?}) -- the resolver is not reading the binding"
            );
        }
        // Direction 2: every gate the LITERAL reader already classified as
        // `=1` still is. Asserted as CONTAINMENT, not equality: a segment may
        // spell the flag twice (`=0` in the base recipe, `=1` as the per-arm
        // override), and the old reader took the last one while the resolver
        // takes the set. The claim being pinned is "still on", which is the
        // direction a regression would lose.
        let mut pinned = 0usize;
        for gate in &gates {
            if !gate.contains("\"--enable-tx-size-search=1\"") {
                continue;
            }
            pinned += 1;
            assert!(
                spelling_values(gate, "enable-tx-size-search").contains(&'1'),
                "a gate that literally spells --enable-tx-size-search=1 is no longer \
                 classified on: {:?}",
                &gate[..gate.len().min(80)]
            );
        }
        assert!(
            pinned >= 10,
            "expected the literal `=1` gates to still be there, found {pinned}"
        );
    }

    /// lane-av1oracleskip: the ORACLE-PRESENCE SCAN — the mechanical
    /// anti-regression for the skip class this lane closed.
    ///
    /// Why a scan and not a review rule: the r2 sweep found FIVE gates that
    /// had been added with the bare `if aomdec_path().is_file() { …compare… }`
    /// shape AFTER the first fix had already routed thirteen sites — every one
    /// of them compiled, passed review and reported green with the oracle
    /// compare silently skipped, under `EC_AV1_REQUIRE_AOMENC=1`. A guard
    /// written the old way re-opens the class the moment it merges, so the
    /// only thing that holds is a check that reads the source itself.
    ///
    /// The rule: a TOOL PRESENCE CHECK (`aomenc_path().is_file()`,
    /// `aomdec_path().is_file()`, `affine_aomenc_path().is_file()`, plain or
    /// `!…` inverted) may appear ONLY
    ///   1. inside the body of the probe that owns that path
    ///      (`have_aomenc`, `aomdec_available`, `have_affine_aomenc`) — that
    ///      is where the assert-last and the env escape live, and
    ///   2. inside an `assert!` (the deliberate hard-failure form), or
    ///   3. in a comment, which cannot execute.
    /// Anything else is a skip with no env escape, and the test fails naming
    /// the exact `file:line`.
    ///
    /// THE FLOOR. A source scan that matches nothing is the false-pass shape
    /// this crate has been bitten by twice (a `format!` brace escape, and
    /// `args.find(')')` stopping at the wrong paren), so the scan refuses to
    /// pass unless it actually inspected a plausible number of sites: the
    /// count is printed and asserted against `MIN_SITES`. A probe that moved
    /// out of the scan's reach, or a matcher broken by a rename, reds here
    /// instead of reporting a clean bill of health.
    #[test]
    fn no_tool_presence_check_outside_its_probe() {
        /// Below this, the scan is not looking at the tree it thinks it is.
        /// Five is the STRUCTURAL floor: three probes (whose existence the span
        /// assert below already pins) plus the two hard `assert!` sites. Doc
        /// comments mentioning the old shape come and go with the prose, so
        /// they are counted but never load-bearing here.
        const MIN_SITES: usize = 5;
        /// (file, the line that opens the probe owning that path)
        const PROBES: &[(&str, &str)] = &[
            ("stream.rs", "fn have_aomenc()"),
            ("stream.rs", "fn aomdec_available("),
            ("stream.rs", "fn have_affine_aomenc()"),
        ];
        const PATHS: &[&str] = &[
            "aomenc_path().is_file()",
            "aomdec_path().is_file()",
            "affine_aomenc_path().is_file()",
        ];
        let files: [(&str, &str); 3] = [
            ("stream.rs", include_str!("stream.rs")),
            ("decode.rs", include_str!("decode.rs")),
            ("encode.rs", include_str!("encode.rs")),
        ];

        // The line range each probe body owns: from its opener to the first
        // following line that closes it at column 0.
        let mut probe_spans: Vec<(usize, usize)> = Vec::new();
        for (name, src) in files {
            let lines: Vec<&str> = src.lines().collect();
            for (probe_file, opener) in PROBES {
                if *probe_file != name {
                    continue;
                }
                let Some(start) = lines.iter().position(|l| l.contains(opener)).map(|i| i + 1)
                else {
                    continue;
                };
                let end = lines
                    .iter()
                    .skip(start)
                    .position(|l| l.trim_end() == "}")
                    .map(|off| start + off)
                    .unwrap_or(lines.len());
                probe_spans.push((start, end));
            }
        }
        assert!(
            probe_spans.len() == PROBES.len(),
            "the oracle-presence scan recognised {} of the {} probe bodies it is meant to \
             exempt ({probe_spans:?}); a renamed or moved probe would silently exempt every \
             site in the crate, so it reds here",
            probe_spans.len(),
            PROBES.len()
        );

        let mut sites = 0usize;
        let mut allowed = 0usize;
        let mut per_pattern: BTreeMap<&str, usize> = PATHS.iter().map(|p| (*p, 0)).collect();
        let mut offenders: Vec<String> = Vec::new();
        for (name, src) in files {
            for (i, line) in src.lines().enumerate() {
                // LONGEST match wins: `affine_aomenc_path().is_file()` CONTAINS
                // `aomenc_path().is_file()`, so a first-match scan credits the
                // affine probe's line to the aomenc pattern and the per-pattern
                // floor below reds on a pattern that is very much present. (It
                // did, on the first run of this test — which is the floor
                // earning its place.)
                let Some(path) = PATHS
                    .iter()
                    .filter(|p| line.contains(**p))
                    .max_by_key(|p| p.len())
                else {
                    continue;
                };
                sites += 1;
                *per_pattern.get_mut(path).expect("pattern came from PATHS") += 1;
                let lineno = i + 1;
                let trimmed = line.trim_start();
                let in_probe = probe_spans
                    .iter()
                    .any(|(start, end)| lineno >= *start && lineno <= *end);
                let in_assert = trimmed.starts_with("assert!")
                    || trimmed.starts_with("aomdec_path().is_file(),")
                    || trimmed.starts_with("aomenc_path().is_file(),")
                    || trimmed.starts_with("affine_aomenc_path().is_file(),");
                let in_comment = trimmed.starts_with("//");
                if in_probe || in_assert || in_comment {
                    allowed += 1;
                } else {
                    offenders.push(format!(
                        "{name}:{lineno}: {}\n      a tool presence check outside its probe — \
                         route it through the probe that owns this path (`have_aomenc`, \
                         `aomdec_available`, `have_affine_aomenc`): a bare `{path}` skips the \
                         gate with NO env escape, so under EC_AV1_REQUIRE_AOMENC=1 the gate still \
                         reports green with the oracle compare never run.",
                        line.trim()
                    ));
                }
            }
        }
        println!(
            "oracle-presence scan: {sites} site(s) inspected, {allowed} in a probe / assert / \
             comment, {} offender(s); per pattern {per_pattern:?}",
            offenders.len()
        );
        // PER-PATTERN floor: a total count can hide one broken matcher (mutate
        // ONE pattern and the other two still add up), so each of the three
        // path patterns must be found at least once -- which the probe bodies
        // guarantee, since each probe contains its own path's presence check.
        let never_matched: Vec<&str> = per_pattern
            .iter()
            .filter(|(_, n)| **n == 0)
            .map(|(p, _)| *p)
            .collect();
        assert!(
            never_matched.is_empty(),
            "the oracle-presence scan matched NOTHING for {never_matched:?}: that pattern is \
             stale (renamed helper, `exists()` instead of `is_file()`, a `format!` escape) and \
             the gates it was meant to police would pass unreviewed. Matched per pattern: \
             {per_pattern:?}"
        );
        assert!(
            sites >= MIN_SITES,
            "the oracle-presence scan inspected only {sites} site(s), below its floor of \
             {MIN_SITES}: the matcher has stopped matching this tree (renamed path helper, a \
             `format!` escape, a moved probe), and with no floor this would pass while scanning \
             nothing — the false-pass shape this crate has been bitten by twice"
        );
        assert!(
            offenders.is_empty(),
            "{} bare tool presence check(s) outside a probe — each one is a gate that can report \
             green without running the oracle compare:\n  {}",
            offenders.len(),
            offenders.join("\n  ")
        );
    }
}

/// lane-av1pins5 r5: the PIN INVENTORY.
///
/// The r3 inventory this replaces matched only two call shapes --
/// `crate_pin("X")` and `pin_dir().join("X")` -- both SINGLE-NAME. A gate that
/// reads a whole DIRECTORY of pins through a `concat!` literal and then formats
/// a name at runtime (`format!("{fixtures}/{n}.obu")`) matched neither, so
/// `pinned_warp_stream_decodes_pixel_exact` and its 14 uncommitted pins were
/// invisible to it, and r3 reported "the machine-local-pin population is zero".
/// It was not. This enumerator resolves all four shapes.
///
/// Ownership, stated because two lanes touch this: THIS scanner owns the
/// ENUMERATION (which gates read which pins, from source). The fixture-preflight
/// lane owns the FILE INVENTORY on a runner (does each referenced path exist on
/// the box). Different questions; both must agree, and this one is the stricter of
/// the two because it fails on an uncommitted pin whether or not some runner
/// happens to have a copy.
#[cfg(test)]
mod pin_inventory {
    /// One pin a gate reads, and where it came from.
    pub struct PinRead {
        pub gate: String,
        pub file: String,
        pub shape: &'static str,
    }

    /// Split `src` into `(fn_name, body)` for every `fn name(` at the gate indent.
    fn gate_bodies(src: &str) -> Vec<(String, &'static str)> {
        let mut out = Vec::new();
        let mut cur: Option<(String, usize)> = None;
        for (i, line) in src.lines().enumerate() {
            let t = line.trim_start();
            if let Some(rest) = t.strip_prefix("fn ") {
                if let Some(name) = rest.split('(').next() {
                    if !name.contains(' ') {
                        cur = Some((name.to_string(), i));
                    }
                }
            }
            // A `#[test]`/`#[ignore]` line is never a closing brace, so the fn
            // ends at the first line that is exactly four-space `}`.
            if let Some((name, start)) = &cur {
                if i > *start && line == "    }" {
                    let body = src
                        .lines()
                        .skip(*start)
                        .take(i - *start + 1)
                        .collect::<Vec<_>>()
                        .join("\n");
                    out.push((name.clone(), &*Box::leak(body.into_boxed_str())));
                    cur = None;
                }
            }
        }
        out
    }

    /// Every pin every gate reads, across all four shapes.
    pub fn pins_read(src: &str) -> Vec<PinRead> {
        let mut out = Vec::new();
        for (gate, body) in gate_bodies(src) {
            // (a) crate_pin("X") and (b) pin_dir().join("X") -- single name.
            for m in body.match_indices("crate_pin(\"") {
                let rest = &body[m.0 + 11..];
                if let Some(e) = rest.find('"') {
                    out.push(PinRead {
                        gate: gate.clone(),
                        file: rest[..e].to_string(),
                        shape: "crate_pin",
                    });
                }
            }
            for m in body.match_indices("pin_dir().join(\"") {
                let rest = &body[m.0 + 16..];
                if let Some(e) = rest.find('"') {
                    out.push(PinRead {
                        gate: gate.clone(),
                        file: rest[..e].to_string(),
                        shape: "pin_dir_machine_local",
                    });
                }
            }
            // (c) a whole-file concat! literal to a named pin:
            //     concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/X.obu")
            for m in body.match_indices("/../../fixtures/") {
                let rest = &body[m.0 + 16..];
                if let Some(e) = rest.find('"') {
                    out.push(PinRead {
                        gate: gate.clone(),
                        file: rest[..e].to_string(),
                        shape: "concat_literal_machine_local",
                    });
                }
            }
            // (d) the shape that hid 14 pins: a DIRECTORY literal bound to a
            //     local, plus a runtime `format!("{var}/{n}.ext")` over a
            //     literal name array. Resolve the array's names.
            for (vi, line) in body.lines().enumerate() {
                let tl = line.trim();
                // `let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures");`
                // -> the bound name is whatever sits between `let ` and the `=`.
                let var = match (
                    tl.find("concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../fixtures\")"),
                    tl.find('='),
                ) {
                    (Some(a), Some(eq)) if eq < a => tl[4..eq].trim().to_string(),
                    _ => continue,
                };
                if var.is_empty() {
                    continue;
                }
                // `format!("{fixtures}/{n}.obu")` -- built by concatenation, not
                // by `format!`: escaping `{{`/`}}` to emit literal braces here is
                // exactly the bug that made this shape resolve to 0 pins on the
                // first run of the scanner.
                let marker = ["format!(\"", "{", &var, "}/{n}"].concat();
                let Some(pos) = body.find(&marker) else {
                    continue;
                };
                // Walk back from the format! to the `[ ... ].iter()` array and
                // take every quoted name in it.
                let head = &body[..pos];
                let Some(bracket) = head.rfind('[') else {
                    continue;
                };
                let names: Vec<String> = head[bracket..]
                    .split('"')
                    .skip(1)
                    .step_by(2)
                    .filter(|s| !s.is_empty() && !s.contains(' ') && *s != "]")
                    .map(|s| s.to_string())
                    .collect();
                // `.obu` -- SLICE up to the closing quote. (Stringifying the
                // `find` index instead produced names like `warp-mismatch4`.)
                let tail = &body[pos + marker.len()..];
                let ext = tail
                    .find('"')
                    .map(|e| tail[..e].to_string())
                    .unwrap_or_default();
                for n in names {
                    out.push(PinRead {
                        gate: gate.clone(),
                        file: format!("{n}{ext}"),
                        shape: "concat_dir_runtime_name",
                    });
                }
                let _ = vi;
            }
            // (e) the shape this very fix INTRODUCED:
            //     `.map(|n| crate_pin(&format!("{n}.obu")))`. Rewriting the
            //     directory literal as a committed-path lookup made the gate
            //     correct and simultaneously invisible -- `crate_pin("` no
            //     longer appears, so shape (a) found nothing and the
            //     enumerator reported a CLEAN tree while 14 pins were read. A
            //     false pass is worse than the original gap: the original at
            //     least showed up in a filesystem audit. Caught here by the
            //     `reads.len()` floor in the invariant test.
            for m in body.match_indices("crate_pin(&format!(") {
                let rest = &body[m.0 + 19..];
                let Some(close) = rest.find(')') else {
                    continue;
                };
                let call = &rest[..close];
                if !call.contains('"') {
                    continue;
                }
                let head = &body[..m.0];
                let Some(bracket) = head.rfind('[') else {
                    continue;
                };
                let names: Vec<String> = head[bracket..]
                    .split('"')
                    .skip(1)
                    .step_by(2)
                    .filter(|x| !x.is_empty() && !x.contains(' ') && *x != "]")
                    .map(|x| x.to_string())
                    .collect();
                // the literal inside the format!, e.g. `{n}.obu`
                let lit = call.split('"').nth(1).unwrap_or("");
                let ext = lit
                    .split_once("}")
                    .map(|(_, e)| e.to_string())
                    .unwrap_or_default();
                for n in names {
                    out.push(PinRead {
                        gate: gate.clone(),
                        file: format!("{n}{ext}"),
                        shape: "crate_pin_runtime_name",
                    });
                }
            }
        }
        out
    }

    /// Does this shape resolve through `crate_pin`, i.e. the COMMITTED
    /// crate-local copy? Both the literal form and the runtime-name form do --
    /// only the two `pin_dir`/root-`concat!` forms are machine-local.
    pub fn resolves_committed(shape: &str) -> bool {
        matches!(shape, "crate_pin" | "crate_pin_runtime_name")
    }

    /// Is `file` present as a committed crate-local fixture?
    pub fn is_committed(file: &str) -> bool {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(file)
            .is_file()
    }
}

#[cfg(test)]
mod pin_inventory_tests {
    use super::pin_inventory::{is_committed, pins_read};

    /// The scanner must be able to SEE the directory-literal shape. Run over a
    /// SYNTHETIC source, not the live tree: r5 removed that shape from
    /// `pinned_warp_stream_decodes_pixel_exact`, so a test that asserted it
    /// against the live file would rot the moment the fix landed -- which is
    /// precisely how a scanner loses its edge and reports a clean tree again.
    /// A capability test must have its own input.
    #[test]
    fn the_pin_scanner_sees_the_directory_literal_shape() {
        let synthetic = r#"
    #[test]
    fn a_gate_using_the_old_shape() {
        let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures");
        let paths: Vec<String> = match std::env::var("EC_AV1_GATE_DUMP_PIN") {
            Ok(p) => vec![p],
            Err(_) => [
                "warp-mismatch",
                "ii-flake-1",
                "rect-flake-3",
            ]
            .iter()
            .map(|n| format!("{fixtures}/{n}.obu"))
            .collect(),
        };
        for path in paths {
            check(&path);
        }
    }
"#;
        let reads = pins_read(synthetic);
        let names: Vec<&str> = reads.iter().map(|r| r.file.as_str()).collect();
        assert_eq!(
            names,
            vec!["warp-mismatch.obu", "ii-flake-1.obu", "rect-flake-3.obu"],
            "the directory-literal + runtime-name shape resolved to {names:?} -- a scanner \
             that cannot see this shape is how 14 uncommitted pins survived three rounds"
        );
        assert!(
            reads.iter().all(|r| r.shape == "concat_dir_runtime_name"),
            "the synthetic pins were attributed to the wrong shape: {:?}",
            reads.iter().map(|r| r.shape).collect::<Vec<_>>()
        );
    }

    /// The other three shapes resolve too -- same reasoning, same synthetic input,
    /// so the capability survives the tree being fixed.
    #[test]
    fn the_pin_scanner_sees_every_shape() {
        let synthetic = r#"
    #[test]
    fn shapes() {
        let a = crate_pin("golden3-pin.obu");
        let b = pin_dir().join("sbpart-pin.obu");
        let c = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/x.obu"));
    }
"#;
        let mut names: Vec<String> = pins_read(synthetic)
            .iter()
            .map(|r| format!("{}|{}", r.file, r.shape))
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "golden3-pin.obu|crate_pin",
                "sbpart-pin.obu|pin_dir_machine_local",
                "x.obu|concat_literal_machine_local",
            ],
            "shape coverage changed: {names:?}"
        );
    }

    /// THE INVARIANT, on the live tree: every pin a gate reads is a COMMITTED
    /// crate-local copy, and NO gate reads through a machine-local shape. This is
    /// the check the r3 manifest could not express.
    #[test]
    fn every_pin_a_gate_reads_is_committed_under_the_crate() {
        let src = include_str!("stream.rs");
        let reads = pins_read(src);
        assert!(
            reads.len() >= 20,
            "the enumerator found only {} pin reads in stream.rs -- it has lost a shape, \
             which is the failure mode that let 14 pins go unseen",
            reads.len()
        );
        let uncommitted: Vec<String> = reads
            .iter()
            .filter(|r| !is_committed(&r.file))
            .map(|r| format!("{}:{} ({})", r.gate, r.file, r.shape))
            .collect();
        assert!(
            uncommitted.is_empty(),
            "{} pin(s) a gate reads have no committed copy under \
             crates/ec-av1/fixtures/ -- each resolves only on a machine whose \
             gitignored root fixtures/ happens to be populated, so a clean checkout \
             silently skips or panics:\n  {}",
            uncommitted.len(),
            uncommitted.join("\n  ")
        );
        let machine_local: Vec<String> = reads
            .iter()
            .filter(|r| !super::pin_inventory::resolves_committed(r.shape))
            .map(|r| format!("{} reads {} via {}", r.gate, r.file, r.shape))
            .collect();
        assert!(
            machine_local.is_empty(),
            "{} pin read(s) still resolve through a machine-local shape instead of \
             `crate_pin` (the committed crate-local copy), so they break on a clean \
             checkout:\n  {}",
            machine_local.len(),
            machine_local.join("\n  ")
        );
    }
}

/// lane-av1pins5 r5: the COUNT-VACUITY enumerator.
///
/// Passing OUR OWN decode's length as the ORACLE's expected frame count
/// (`ffmpeg_decode_sequence(&stream, w, h, ours.len())`) is a real defect, but
/// NOT the one r4 claimed. MEASURED, by calling
/// `ffmpeg_decode_sequence(&stream, 192, 128, 0)` on a real pinned stream under
/// `catch_unwind`: it PANICS at `stream.rs:5110` with
/// `expected 0 4:2:0 frames, ffmpeg said: `. The helper asserts
/// `out.stdout.len() == frame_bytes * frames` unconditionally, so a wrong count
/// cannot pass silently -- it reds, with a message that blames FFMPEG for a
/// count OUR decoder chose.
///
/// TWO DEFECT CLASSES, DO NOT CONFLATE THEM (lane-av1pins5 r6):
///   * MISATTRIBUTED RED -- a wrong count reds, and the old message blamed
///     ffmpeg for a number OUR decoder chose. This is what these sites are.
///     `ffmpeg_decode_sequence`'s `stdout.len() == frame_bytes * frames` assert
///     is unconditional, so it cannot pass silently.
///   * SILENT PASS -- the compare is skipped or truncated and the gate reports
///     green. A DIFFERENT defect with a DIFFERENT fix, and r4 mislabelled these
///     sites as belonging to it. If you are reading this to decide what to fix,
///     fix the message and the count SOURCE; do not go looking for a silent pass.
///
/// r4 reported this shape as "a vacuous pass" and told Main so. That was wrong.
/// The consequence is a MISATTRIBUTED RED. The fix r4 shipped is still right (the
/// count now comes from the fixture, so the red names the real cause), but the
/// justification was wrong, and a sweep that repeats it would send the next
/// reader after a silent-pass bug that does not exist.
///
/// This enumerator finds every such site and classifies it, so the class is
/// visible and cannot shrink unnoticed.
#[cfg(test)]
mod count_vacuity {
    /// One oracle call whose expected frame count is locally derived.
    pub struct Site {
        pub line: usize,
        pub gate: String,
        pub count_expr: String,
        /// An independent count assertion on the same binding, within a few
        /// lines above: that is what makes the count a SPEC rather than a
        /// self-fulfilling number.
        pub pinned_by: Option<String>,
        /// Does this gate actually COMPARE PIXELS (so a wrong count would
        /// corrupt a verdict), or is it a report-only probe?
        pub pixel_comparing: bool,
        /// A frame count already in scope that is a SPEC rather than a
        /// self-fulfilling number -- e.g. the `FRAMES` the gate encoded, or a
        /// count asserted on the pinned stream. `Some(..)` means the fix is a
        /// one-line swap at this site.
        pub fixable_from: Option<String>,
    }

    pub fn sites(src: &str) -> Vec<Site> {
        let mut out = Vec::new();
        let lines: Vec<&str> = src.lines().collect();
        let mut gate = String::new();
        let mut fn_start = 0usize;
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim_start();
            if let Some(rest) = t.strip_prefix("fn ") {
                if let Some(n) = rest.split('(').next() {
                    if !n.contains(' ') {
                        gate = n.to_string();
                        fn_start = i;
                    }
                }
            }
            for callee in [
                "ffmpeg_decode_sequence(",
                "ffmpeg_decode_sequence_10bit(",
                "ffmpeg_decode_sequence_444(",
            ] {
                let Some(at) = line.find(callee) else {
                    continue;
                };
                let args = &line[at + callee.len()..];
                // DEPTH-AWARE close: `args.find(')')` stops at the `)` of
                // `pictures.len()` and truncates the arg list mid-expression,
                // which silently yielded zero sites on the first run.
                let mut depth = 1usize;
                let mut close = None;
                for (k, ch) in args.char_indices() {
                    match ch {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                close = Some(k);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let Some(close) = close else { continue };
                let arglist = &args[..close];
                let last = arglist.rsplit(',').next().unwrap_or("").trim();
                if !(last.ends_with(".len()") || last.ends_with(".count()")) {
                    continue;
                }
                // Is the SAME binding count-asserted against a literal/const
                // within the preceding 12 lines? That is the difference between
                // a spec and a self-fulfilling number.
                let binding = last.trim_end_matches(".len()").trim_end_matches(".count()");
                // Match the assertion across a LINE BREAK: rustfmt puts
                //   assert_eq!(
                //       frames.len(),
                // so requiring `assert_eq!(frames.len()` on one line missed
                // every multi-line spelling and inflated the unpinned count.
                let want = format!("{binding}.len()");
                let mut pinned = None;
                for back in 1..=14usize {
                    if i < back {
                        break;
                    }
                    let prev = lines[i - back].trim_start();
                    if !prev.starts_with("assert") || !prev.contains('(') {
                        continue;
                    }
                    // rustfmt splits `assert_eq!(` from its first argument, so
                    // the binding is on THIS line or the NEXT one.
                    let here = prev.contains(&want);
                    let next = lines
                        .get(i - back + 1)
                        .map(|l| l.trim_start().contains(&want))
                        .unwrap_or(false);
                    if here || next {
                        pinned = Some(prev.chars().take(60).collect());
                        break;
                    }
                }
                // Pixel-comparing? A gate that compares planes is the only
                // place a wrong count corrupts a verdict; a report-only probe
                // just prints.
                // Bound to THIS fn only. Slicing to EOF matched a later gate's
                // `const FRAMES` and its `assert_eq!(got.y`, which reported all
                // 48 sites as pixel-comparing with a spec in scope -- the same
                // over-broad-scope bug the class keeps teaching.
                // Brace-depth walk, not `find("\n    }")`: a gate with an
                // early 4-space `}` (a bare block, a `match` arm tail) truncated
                // the slice and made the compare-detection read false.
                // STRING-AWARE brace walk. Counting raw bytes counts a `}`
                // inside a format string as a closing brace, which truncated the
                // body and made every compare-detection read false -- the same
                // trap that made stream.rs look 2 braces short in the first
                // review of this wave.
                let bytes = src.as_bytes();
                let mut depth = 0i32;
                let mut started = false;
                let mut fn_end = src.len();
                let mut in_str = false;
                let mut in_line_comment = false;
                let mut k = fn_start;
                while k < bytes.len() {
                    let c = bytes[k];
                    if in_line_comment {
                        if c == b'\n' {
                            in_line_comment = false;
                        }
                        k += 1;
                        continue;
                    }
                    if in_str {
                        if c == b'\\' {
                            k += 2;
                            continue;
                        }
                        if c == b'"' {
                            in_str = false;
                        }
                        k += 1;
                        continue;
                    }
                    match c {
                        b'"' => in_str = true,
                        b'/' if bytes.get(k + 1) == Some(&b'/') => {
                            in_line_comment = true;
                            k += 1;
                        }
                        b'{' => {
                            depth += 1;
                            started = true;
                        }
                        b'}' => {
                            depth -= 1;
                            if started && depth == 0 {
                                fn_end = k + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    k += 1;
                }
                let body = &src[fn_start..fn_end];
                // APPROXIMATE, and known to be so: the body slice is a
                // hand-rolled brace walk, and this crate's compare idioms vary
                // enough that the column under-reports. The LINE, GATE and
                // COUNT EXPR columns are exact (line-based scan, no body
                // slicing); treat `pixel_comparing` as a hint, not a verdict.
                // Idiom-tolerant: this crate spells the compare every which way
                // (`assert_eq!(got.y, want.y)`, `assert_eq!(g, w)`,
                // `let ok = ours.y == ref_f.y`, `ours.y, want.y`), so match the
                // PLANE inside an assertion, not one fixed spelling.
                let pixel_comparing = body.contains("assert")
                    && (body.contains(".y,")
                        || body.contains(".y ==")
                        || body.contains(".y)")
                        || body.contains("vs ffmpeg"));
                // A SPEC already in scope for this gate?
                let mut fixable = None;
                for cand in ["FRAMES", "frame_count", "NFRAMES", "frames_expected"] {
                    if body.contains(&format!("const {cand}"))
                        || body.contains(&format!("let {cand}"))
                    {
                        fixable = Some(cand.to_string());
                        break;
                    }
                }
                out.push(Site {
                    line: i + 1,
                    gate: gate.clone(),
                    count_expr: last.to_string(),
                    pinned_by: pinned,
                    pixel_comparing,
                    fixable_from: fixable,
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod count_vacuity_tests {
    use super::count_vacuity::sites;

    /// The shape is real and widespread; this pins the FLOOR so the sweep cannot
    /// silently stop finding sites. Measured count is well above this.
    #[test]
    fn the_count_vacuity_sweep_finds_the_known_sites() {
        let src = include_str!("stream.rs");
        let s = sites(src);
        assert!(
            s.len() >= 40,
            "the count-vacuity sweep found only {} site(s); expected 40+ -- it has lost a \
             shape",
            s.len()
        );
        assert!(
            s.iter()
                .any(|x| x.line == 3895 || x.count_expr == "pictures.len()"),
            "the original reported site (pictures.len()) is gone -- re-derive the sweep"
        );
    }

    /// Report, per site: does the count come from a SPEC or from our own decode?
    /// Prints the table the r5 report carries. The assertion is the FLOOR above,
    /// not "zero unpinned" -- the residual unpinned sites are handed to another
    /// lane with this table, and forcing them all here would be an unbounded
    /// change dressed as a fix.
    #[test]
    fn every_locally_derived_oracle_count_is_reported() {
        let src = include_str!("stream.rs");
        let s = sites(src);
        let unpinned: Vec<String> = s
            .iter()
            .filter(|x| x.pinned_by.is_none())
            .map(|x| {
                format!(
                    "stream.rs:{} | {} | {} | NOT SPEC-PINNED",
                    x.line, x.gate, x.count_expr
                )
            })
            .collect();
        let pinned = s.len() - unpinned.len();
        eprintln!(
            "count-vacuity sweep: {} site(s); {} spec-pinned by a prior assert, {} not",
            s.len(),
            pinned,
            unpinned.len()
        );
        for line in unpinned.iter() {
            eprintln!("  {line}");
        }
        // The table Main asked for, as columns the guard itself computes.
        for x in s.iter().filter(|x| x.pinned_by.is_none()) {
            eprintln!(
                "ROW {} | {} | {} | pixel_comparing={} | spec_in_scope={}",
                x.line,
                x.gate,
                x.count_expr,
                x.pixel_comparing,
                x.fixable_from.clone().unwrap_or_else(|| "-".into())
            );
        }
        assert!(
            !s.is_empty() && s.iter().all(|x| !x.gate.is_empty()),
            "a site has no owning gate -- the sweep's gate attribution is broken"
        );
    }
}
