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
/// the same shape.
///
/// The "all 49" count is GONE as of 2026-09-29 (lane-av1distwtd r2) and is
/// deliberately not replaced by a number: the tool is no longer pinned off
/// tree-wide, so any count would be a snapshot that rots the same way.
/// Measured on this tree for the record: 113 of the 146 selected gate bodies
/// name it, 88 pass `=0`, 15 pass `=1`, and 9 build the value into a
/// `format!` variable the census cannot see -- so the historical claim no
/// longer holds in EITHER direction, and the `enable-tx-size-search` entry
/// below is a live question, not a settled one.
///
/// Each entry is `(tool, spellings, aomenc default, what counts as on)`. The
/// "defaulted means unknown" rule of [`NEVER_EXERCISED`] applies unchanged: a
/// default of `1` does not prove the encoder picked the tool for any stream, so
/// only a gate that spells an on-value at that bit depth retires an entry.
    // lane-av1distwtd r3: `enable-tx-size-search` is the one DEFAULT_ON_TOOLS
    // entry the r2 audit could NOT settle, and the reason is a THIRD detector
    // limit rather than a coverage fact. After the r3 call-resolving fix, 113
    // of the 230 selected gates name it: 88 pass `=0`, 15 pass `=1`, and 9
    // build the value into a `format!`/`String` variable that neither
    // `flags_in` nor `settings_in` can see, because both scan for a literal
    // `"--flag=value"` inside one segment.
    //
    // WHAT WOULD SETTLE IT, for whoever takes it next -- the method, not a
    // guess: extend `flags_in`/`settings_in` to resolve a local binding, the
    // same way `gate_bodies` now resolves calls. Concretely, when a segment
    // contains `let <name> = format!("--enable-tx-size-search={tx_search}")`
    // (or a `String::from`/`to_string` of the same shape), record the
    // TEMPLATE and bind the variable's value at each use site. Re-count, and
    // only then decide whether the tool is a live hole. Until that exists the
    // count above is a floor, not a measurement: 9 gates are unclassified in
    // an unknown direction, and an unknown direction is exactly the failure
    // mode this file exists to prevent.
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
        On, ALIASES, DEFAULT_ON_TOOLS, NEVER_EXERCISED, NEVER_EXERCISED_10BIT,
        NEVER_EXERCISED_8BIT, NEVER_ON_10BIT, NEVER_ON_8BIT, TOOL_UNIVERSE,
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

    /// Whether a gate body spells one [`DEFAULT_ON_TOOLS`] entry on / off.
    fn default_on_state(gate: &str, spellings: &[&str], on: On) -> Option<bool> {
        let here = settings_in(gate);
        let mut state = None;
        for spelling in spellings {
            let Some(value) = here.get(*spelling) else {
                continue;
            };
            let Ok(value) = value.parse::<u32>() else {
                continue;
            };
            let is_on = match on {
                On::NonZero => value != 0,
                On::AtMost(limit) => value <= limit,
            };
            // Any spelling that says "on" wins: --tile-columns=0 --tile-rows=1
            // is a multi-tile stream.
            state = Some(state.unwrap_or(false) || is_on);
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
            .find(|b| {
                b.contains("a_real_aomenc_stream_with_a_1d_tx_class_on_a_rect_transform")
            })
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
            let states: Vec<Option<bool>> = gates
                .iter()
                .map(|g| default_on_state(g, spellings, on))
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
}
