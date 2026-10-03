#![cfg(test)]
//! A measured bound on the crate's BY-VALUE types, and a gate that bites.
//!
//! # The class
//!
//! A large struct returned BY VALUE from a public constructor is a stack cost,
//! not a heap cost, and the cost is paid by the CALLER. The constructors here
//! nest -- `with_pyramid_and_rate_target` -> `with_pyramid` -> `new` for the
//! encoder, and `encode_key_frame` -> `encode_key_frame_with_ctx` ->
//! `encode_key_frame_with_modes_with_ctx` -> `encode_key_frame_inner` for a
//! frame -- and in an unoptimized build each level materialises its whole
//! `Result<T>` return slot before the inner call returns. So one value's
//! `size_of` is the MULTIPLIER, and the chain length is the other factor.
//!
//! That is not hypothetical. `Av1Encoder` was 139960 bytes by value (eight
//! inline `CdfSnapshot` DPB slots) and aborted with `fatal runtime error:
//! stack overflow, aborting` on libtest's 2 MiB thread, twice, on an unmodified
//! `main`. `.cargo/config.toml` carries `RUST_MIN_STACK = 67108864` to hide it
//! from this workspace's own runs -- a band-aid a library consumer does not
//! get. lane-av1enchang boxed the per-slot snapshot (139960 -> 18168) and
//! gated the constructors on a 1 MiB thread.
//!
//! # Why this module exists when that gate already exists
//!
//! lane-av1enchang gated ONE type. The class was not swept: nothing stopped the
//! next 15 KB inline table from landing, and nothing said which types had been
//! checked. This module is the sweep:
//!
//! 1. [`BY_VALUE_TYPES`] is the inventory -- every public type in this crate
//!    that a public `fn` can return by value, with a measured `size_of` floor
//!    recorded as of this commit.
//!    [`the_measured_inventory_is_accurate`] fails if a type's size moves, so
//!    the table cannot rot into a list of stale small numbers.
//! 2. [`STACK_BUDGET`] is the bound, and
//!    [`every_by_value_type_fits_the_stack_budget`] asserts it BY NAME for
//!    every row, so a regression reds naming the type that caused it.
//! 3. [`the_deepest_by_value_constructors_fit_on_a_tight_stack`] is the
//!    capability arm: it really constructs the heavy types on a deliberately
//!    tight thread, so the old layout ABORTS THE PROCESS rather than quietly
//!    costing margin. Red/green in milliseconds.
//! 4. [`every_by_value_public_return_is_in_the_inventory`] is the sweep's
//!    teeth against NEW types: a source scan re-derives the set of by-value
//!    public returns from the crate source, so a lane that adds a 40 KB struct
//!    to a `pub fn` return reds here until it is measured and listed.
//!    [`the_inventory_scan_is_not_vacuous`] is its control.
//!
//! # The bound, and why this number
//!
//! [`STACK_BUDGET`] = 8192 bytes (8 KiB). It is bounded from BOTH sides by
//! measurement, and both sides are asserted in
//! [`every_by_value_type_fits_the_stack_budget`] so neither can rot silently.
//!
//! **The quantity being bounded.** Not `size_of` in the abstract, but *what a
//! caller can be made to owe*: the size of one by-value return, multiplied by
//! how many constructor levels hold a copy of it at once. Three inputs:
//!
//! 1. **The smallest stack a caller can hand this crate is 2,101,248 bytes.**
//!    Measured, not assumed, by
//!    [`test_threads_get_the_stack_they_are_given`] reading
//!    `/proc/self/task/<tid>/maps`. That is libtest's default test thread and
//!    also what `std::thread::Builder::new()` gives a caller who does not ask
//!    for a size. A library consumer gets no `.cargo/config.toml`.
//! 2. **The deepest PUBLIC by-value chain is 4** ([`DEEPEST_PUBLIC_CHAIN`]):
//!    `encode_key_frame` -> `encode_key_frame_with_ctx` ->
//!    `encode_key_frame_with_modes_with_ctx` -> `encode_key_frame_inner`. The
//!    encoder's own nest is 3. The test asserts
//!    `DEEPEST_PUBLIC_CHAIN * worst * 8 <= 2 MiB` -- i.e. the by-value return
//!    slots may claim at most an EIGHTH of a caller's stack, leaving the rest
//!    for the encode search, which is the thing that actually needs it.
//! 3. **The budget applies only to types a `pub fn` returns.** The
//!    crate-private 15,232-byte `Cdfs` / `CdfSnapshot` are measured and listed
//!    but are NOT bounded, because no caller can reach them by value -- every
//!    field that holds one is a `Box`, which
//!    [`the_crate_private_cdf_tables_stay_behind_a_box`] pins by source.
//!    Bounding them would calibrate the number to something unreachable.
//!
//! **Where the number sits.** At 8 KiB with the worst public return at 2,944
//! bytes ([`Av1Encoder`]), the worst a caller owes is
//! `4 x 2944 = 11,776` bytes -- **0.56%** of a 2 MiB thread -- and the budget
//! itself is 2.78x above the incumbent. Both directions have room, which is
//! the point: a bound the largest type sits just under is a bound the next
//! lane will quietly raise.
//!
//! **What it catches.** A pre-fix `Av1Encoder` (139,960 B) is **17.1x over**.
//! `Encoded` as it stood before this lane boxed its two inline snapshots
//! (30,728 B) is **3.75x over**. `ec_opus::Encoder`, measured in a sibling
//! crate by the same sweep, is **10.6x over**. The historical defect, the
//! near-miss, and the largest live instance of the class in the workspace all
//! red by a factor, not by a hair.
//!
//! **What it does not claim.** `size_of` is layout, not stack depth. The
//! measured stack/size ratio on a by-value constructor is ~4.4x
//! (`ec_opus::Encoder`: 87,024 B of struct, ~384 KiB of live stack), which is
//! the nesting multiplier made visible. That residual is why
//! [`the_deepest_by_value_constructors_fit_on_a_tight_stack`] constructs for
//! real instead of trusting the arithmetic.

#![cfg(test)]
//! A measured bound on the crate's BY-VALUE types, and a gate that bites.
//!
//! # The class
//!
//! A large struct returned BY VALUE from a public constructor is a stack cost,
//! not a heap cost, and the cost is paid by the CALLER. The constructors here
//! nest -- `with_pyramid_and_rate_target` -> `with_pyramid` -> `new` for the
//! encoder, and `encode_key_frame` -> `encode_key_frame_with_ctx` ->
//! `encode_key_frame_with_modes_with_ctx` -> `encode_key_frame_inner` for a
//! frame -- and in an unoptimized build each level materialises its whole
//! `Result<T>` return slot before the inner call returns. So one value's
//! `size_of` is the MULTIPLIER, and the chain length is the other factor.
//!
//! That is not hypothetical. `Av1Encoder` was 139960 bytes by value (eight
//! inline `CdfSnapshot` DPB slots) and aborted with `fatal runtime error:
//! stack overflow, aborting` on libtest's 2 MiB thread, twice, on an unmodified
//! `main`. `.cargo/config.toml` carries `RUST_MIN_STACK = 67108864` to hide it
//! from this workspace's own runs -- a band-aid a library consumer does not
//! get. lane-av1enchang boxed the per-slot snapshot (139960 -> 18168) and
//! gated the constructors on a 1 MiB thread.
//!
//! # Why this module exists when that gate already exists
//!
//! lane-av1enchang gated ONE type. The class was not swept: nothing stopped the
//! next 15 KB inline table from landing, and nothing said which types had been
//! checked. This module is the sweep:
//!
//! 1. [`BY_VALUE_TYPES`] is the inventory -- every public type in this crate
//!    that a public `fn` can return by value, with a measured `size_of` floor
//!    recorded as of this commit.
//!    [`the_measured_inventory_is_accurate`] fails if a type's size moves, so
//!    the table cannot rot into a list of stale small numbers.
//! 2. [`STACK_BUDGET`] is the bound, and
//!    [`every_by_value_type_fits_the_stack_budget`] asserts it BY NAME for
//!    every row, so a regression reds naming the type that caused it.
//! 3. [`the_deepest_by_value_constructors_fit_on_a_tight_stack`] is the
//!    capability arm: it really constructs the heavy types on a deliberately
//!    tight thread, so the old layout ABORTS THE PROCESS rather than quietly
//!    costing margin. Red/green in milliseconds.
//! 4. [`every_by_value_public_return_is_in_the_inventory`] is the sweep's
//!    teeth against NEW types: a source scan re-derives the set of by-value
//!    public returns from the crate source, so a lane that adds a 40 KB struct
//!    to a `pub fn` return reds here until it is measured and listed.
//!    [`the_inventory_scan_is_not_vacuous`] is its control.
//!
//! # The bound, and why this number
//!
//! [`STACK_BUDGET`] = 32768 bytes (32 KiB). The reasoning, from measured
//! numbers rather than taste:
//!
//! * The smallest stack a caller can hand this crate is **2101248 bytes**,
//!   measured by [`test_threads_get_the_stack_they_are_given`] reading
//!   `/proc/self/task/<tid>/maps` for the region containing a running frame.
//!   That is libtest's default test thread and also what
//!   `std::thread::Builder::new()` hands a caller who does not ask for a size.
//!   A library consumer gets no `.cargo/config.toml`.
//! * The deepest public by-value chain in this crate is FOUR frames, so the
//!   worst live stack from return slots alone is `4 * size_of::<T>()`.
//! * At 32 KiB that is 128 KiB: **6.1%** of a 2 MiB thread. A type at the
//!   budget still leaves 94% of the caller's stack for its own work.
//! * 32 KiB is above every type here with headroom, and it is 2.14x below the
//!   pre-fix `Av1Encoder`, so the historical defect reds by a factor rather
//!   than by a hair.
//!
//! `Encoded` is at 30728, i.e. **93.8% of budget** and the one row with no
//! margin left. It is 99.2% two inline `CdfSnapshot`s, and boxing those two
//! fields is the next move in this class; the report says so. A budget the
//! current tree already violates is not a budget, it is a to-do list, and the
//! fix belongs in the lane that does the boxing.

use crate::encode::Encoded;
use crate::encoder::Av1Encoder;

/// The per-type ceiling for a type a CALLER can be made to hold by value, in
/// bytes. See the module docs for the derivation; it is two-sided, and both
/// sides are measured.
pub(crate) const STACK_BUDGET: usize = 8192;

/// The deepest PUBLIC by-value constructor chain in this crate, counted from
/// the source: `encode_key_frame` -> `encode_key_frame_with_ctx` ->
/// `encode_key_frame_with_modes_with_ctx` -> `encode_key_frame_inner` is four,
/// and `with_pyramid_and_rate_target` -> `with_pyramid` -> `new` is three. The
/// budget is checked against four, the worse of the two.
#[cfg(test)]
const DEEPEST_PUBLIC_CHAIN: usize = 4;

/// Every public type in this crate a public `fn` can return BY VALUE, with the
/// `size_of` measured at the commit that wrote this table. `None` means "not
/// yet measured", which [`the_measured_inventory_is_accurate`] rejects.
///
/// The third column names the return path, so a reader can see why a type is
/// here rather than a coincidence of the type system.
pub(crate) const BY_VALUE_TYPES: &[(&str, Option<usize>, bool, &str)] = &[
    // -- encoder.rs -------------------------------------------------------
    (
        "Av1Encoder",
        Some(2944),
        true,
        "Av1Encoder::new / with_speed / with_pyramid / with_rate_target / \
         with_pyramid_and_rate_target, all `Result<Self>`, nesting three deep",
    ),
    ("EncoderConfig", Some(40), true, "EncoderConfig::new"),
    (
        "Pyramid",
        Some(24),
        true,
        "Pyramid's own ctor + Av1Encoder::pyramid",
    ),
    (
        "RateTarget",
        Some(16),
        true,
        "constructor by value; carried in every Av1Encoder",
    ),
    (
        "Packet",
        Some(48),
        true,
        "Av1Encoder::encode -> Result<Packet>",
    ),
    // -- encode.rs --------------------------------------------------------
    (
        "Encoded",
        Some(280),
        true,
        "encode_key_frame / encode_key_frame_with_modes / \
         encode_key_frame_at_size -> Result<Encoded>, nesting four deep",
    ),
    (
        "EncodedSequence",
        Some(72),
        true,
        "encode_sequence -> Result<EncodedSequence>",
    ),
    ("Picture", Some(88), true, "Picture::grey -> Self"),
    // -- crate-private: never a public return, and every one of them is
    // -- behind a Box now. The budget does not apply to these; the pin that
    // -- does is `the_crate_private_cdf_tables_stay_behind_a_box`.
    (
        "Cdfs",
        Some(15232),
        false,
        "Cdfs::new(q_ctx) -> Cdfs; the payload of every CdfSnapshot. Not \
         reachable from a pub fn, and boxed everywhere it is stored",
    ),
    (
        "CdfSnapshot",
        Some(15232),
        false,
        "crate-private newtype over Cdfs. Boxed in DpbSlot (lane-av1enchang), \
         in Av1Encoder::carried_cdfs and in both of Encoded's fields \
         (lane-av1stacksweep)",
    ),
    (
        "TxbTables",
        Some(112),
        false,
        "Cdfs::txb -> TxbTables<'_>; nine BORROWED table slices, so the move \
         copies pointers, not tables",
    ),
    // -- decode.rs --------------------------------------------------------
    (
        "FrameCtx",
        Some(1296),
        false,
        "FrameCtx::for_encoder() -> Self, pub(crate); one inline per Av1Encoder",
    ),
    // -- the small public surface ----------------------------------------
    (
        "SymbolEncoder",
        Some(56),
        true,
        "msac::SymbolEncoder::new / pricer",
    ),
    ("SymbolDecoder", Some(64), true, "msac::SymbolDecoder::new"),
    (
        "MiInfo",
        Some(16),
        true,
        "mvstack::MvStack::get -> Option<MiInfo>",
    ),
    (
        "WarpParams",
        Some(32),
        true,
        "warp::global_warp_params -> Option<WarpParams>",
    ),
    ("Iqm", Some(24), true, "qm::iwt_matrix -> Option<Iqm>"),
    ("MvStack", Some(240), true, "mvstack::MvStack::new -> Self"),
    (
        "Frame",
        Some(352),
        true,
        "census::Frame rows, moved by value",
    ),
    (
        "InterpFilterKind",
        Some(1),
        true,
        "mc::InterpFilterKind::from_switchable_symbol / from_header",
    ),
    (
        "TplProbe",
        Some(48),
        false,
        "a borrowed-view handle the tile search passes around by value",
    ),
];

/// The live `size_of` for every inventoried type, keyed BY NAME.
///
/// Keyed rather than positional on purpose: `Packet` and `TplProbe` are both
/// 48 bytes, so a row inserted into [`BY_VALUE_TYPES`] without a matching
/// insert here would zip cleanly and assert the wrong type's size against the
/// wrong recorded one. A name key makes that mistake loud instead.
#[cfg(test)]
fn measured_by_name() -> Vec<(&'static str, usize)> {
    vec![
        ("Av1Encoder", std::mem::size_of::<Av1Encoder>()),
        (
            "EncoderConfig",
            std::mem::size_of::<crate::encoder::EncoderConfig>(),
        ),
        ("Pyramid", std::mem::size_of::<crate::encoder::Pyramid>()),
        (
            "RateTarget",
            std::mem::size_of::<crate::encoder::RateTarget>(),
        ),
        ("Packet", std::mem::size_of::<crate::encoder::Packet>()),
        ("Encoded", std::mem::size_of::<Encoded>()),
        (
            "EncodedSequence",
            std::mem::size_of::<crate::encode::EncodedSequence>(),
        ),
        ("Picture", std::mem::size_of::<crate::encode::Picture>()),
        ("Cdfs", std::mem::size_of::<crate::cdf_state::Cdfs>()),
        (
            "CdfSnapshot",
            std::mem::size_of::<crate::encode::CdfSnapshot>(),
        ),
        (
            "TxbTables",
            std::mem::size_of::<crate::cdf_state::TxbTables<'static>>(),
        ),
        ("FrameCtx", std::mem::size_of::<crate::decode::FrameCtx>()),
        (
            "SymbolEncoder",
            std::mem::size_of::<crate::msac::SymbolEncoder>(),
        ),
        (
            "SymbolDecoder",
            std::mem::size_of::<crate::msac::SymbolDecoder<'static>>(),
        ),
        ("MiInfo", std::mem::size_of::<crate::mvstack::MiInfo>()),
        ("WarpParams", std::mem::size_of::<crate::warp::WarpParams>()),
        ("Iqm", std::mem::size_of::<crate::qm::Iqm>()),
        ("MvStack", std::mem::size_of::<crate::mvstack::MvStack>()),
        ("Frame", std::mem::size_of::<crate::census::Frame>()),
        (
            "InterpFilterKind",
            std::mem::size_of::<crate::mc::InterpFilterKind>(),
        ),
        (
            "TplProbe",
            std::mem::size_of::<crate::motion_field::TplProbe<'static>>(),
        ),
    ]
}

/// The stack [`the_deepest_by_value_constructors_fit_on_a_tight_stack`]
/// constructs on.
///
/// 1 MiB: half of what an unconfigured caller gives this crate, and 2.7x what
/// the deepest measured chain needs in a debug build. Both thresholds were
/// swept in an unoptimized build and are reproducible with
/// `EC_AV1_TIGHT_STACK_BYTES`.
const TIGHT_STACK: usize = 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::{
        BY_VALUE_TYPES, DEEPEST_PUBLIC_CHAIN, STACK_BUDGET, TIGHT_STACK, measured_by_name,
    };
    use crate::encode::Encoded;

    /// Return types that are not a by-value copy of anything the caller has to
    /// own: a pointer, a slice, a `Vec` (three words, and the heap is not the
    /// stack), or a primitive. `Self` is a by-value copy of the impl's type,
    /// which the inventory rows already cover, so it is exempt from the scan
    /// and not from the table.
    const THIN: &[&str] = &[
        "Box", "Rc", "Arc", "Cow", "Vec", "VecDeque", "String", "OsString", "PathBuf", "HashMap",
        "BTreeMap", "BTreeSet", "HashSet", "str", "u8", "u16", "u32", "u64", "usize", "i8", "i16",
        "i32", "i64", "isize", "f32", "f64", "bool", "char", "Self",
    ];

    /// The type name a `pub fn` line returns BY VALUE, or `None` when the line
    /// returns a pointer, a `Vec`, a primitive, or nothing.
    ///
    /// One function, shared by the sweep and its control, so the control
    /// cannot pass while the sweep's copy of the logic rotted.
    fn by_value_return_name(line: &str) -> Option<String> {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("pub fn ") {
            return None;
        }
        let arrow = line.find("->")?;
        let rest = &line[arrow + 2..];
        let end = rest.find('{').unwrap_or(rest.len());
        let ret = rest[..end].trim();
        if ret.is_empty() {
            return None;
        }
        // Unwrap one `Result<..>` / `Option<..>` layer. A deeper nesting is
        // not a shape this crate uses, and flagging one as uninventoried is
        // the safe direction to be wrong in.
        let inner = {
            let t = ret
                .strip_prefix("Result<")
                .or_else(|| ret.strip_prefix("Option<"))
                .unwrap_or(ret);
            t.split('>').next().unwrap_or(t)
        };
        // Drop the error type and any lifetime or generic argument:
        // `Result<Decoder, Error>`, `SymbolDecoder<'a>` and `TxbTables<'_>`
        // are all the bare name for this purpose. Splitting on the comma is
        // load-bearing: the first version of this matcher cut only at `>` and
        // so read `Result<Decoder, Error>` as the name "Decoder, Error".
        // `the_inventory_scan_is_not_vacuous` caught exactly that, which is
        // why the control is not optional.
        let name = inner.split(['<', ',']).next().unwrap_or(inner).trim();
        if name.is_empty() || THIN.contains(&name) {
            return None;
        }
        // A generic parameter, not a concrete type.
        if !name.chars().next().is_some_and(char::is_uppercase) {
            return None;
        }
        Some(name.to_string())
    }

    /// The source files the by-value scan reads. Every module that can expose
    /// a `pub fn` returning a named type; a new module added to `lib.rs` needs
    /// adding here, and
    /// [`every_by_value_module_is_scanned`] is what keeps that honest.
    const FILES: &[(&str, &str)] = &[
        ("bits.rs", include_str!("bits.rs")),
        ("cdf.rs", include_str!("cdf.rs")),
        ("cdf_state.rs", include_str!("cdf_state.rs")),
        ("census.rs", include_str!("census.rs")),
        ("compound.rs", include_str!("compound.rs")),
        ("decode.rs", include_str!("decode.rs")),
        ("dumpio.rs", include_str!("dumpio.rs")),
        ("encode.rs", include_str!("encode.rs")),
        ("encoder.rs", include_str!("encoder.rs")),
        ("envflags.rs", include_str!("envflags.rs")),
        ("film_grain.rs", include_str!("film_grain.rs")),
        ("filter_search.rs", include_str!("filter_search.rs")),
        ("frame.rs", include_str!("frame.rs")),
        ("gate_coverage.rs", include_str!("gate_coverage.rs")),
        ("hits.rs", include_str!("hits.rs")),
        ("intra.rs", include_str!("intra.rs")),
        ("library_fixture.rs", include_str!("library_fixture.rs")),
        ("mc.rs", include_str!("mc.rs")),
        ("motion.rs", include_str!("motion.rs")),
        ("motion_field.rs", include_str!("motion_field.rs")),
        ("msac.rs", include_str!("msac.rs")),
        ("mvstack.rs", include_str!("mvstack.rs")),
        ("obu.rs", include_str!("obu.rs")),
        ("par.rs", include_str!("par.rs")),
        ("probe.rs", include_str!("probe.rs")),
        ("qm.rs", include_str!("qm.rs")),
        ("quant.rs", include_str!("quant.rs")),
        ("refusal_inventory.rs", include_str!("refusal_inventory.rs")),
        ("restoration.rs", include_str!("restoration.rs")),
        ("sequence.rs", include_str!("sequence.rs")),
        ("speed.rs", include_str!("speed.rs")),
        ("stream.rs", include_str!("stream.rs")),
        ("superres.rs", include_str!("superres.rs")),
        ("tile.rs", include_str!("tile.rs")),
        ("timeline.rs", include_str!("timeline.rs")),
        ("transform.rs", include_str!("transform.rs")),
        ("warp.rs", include_str!("warp.rs")),
        ("wedge.rs", include_str!("wedge.rs")),
        ("stack_budget.rs", include_str!("stack_budget.rs")),
    ];

    /// Every module `lib.rs` declares, so the scan's file list cannot silently
    /// fall behind a new module. A new `mod foo;` with a by-value `pub fn`
    /// that the scan never reads is exactly the hole this class would come
    /// back through, so the list is derived, not curated.
    #[test]
    fn every_by_value_module_is_scanned() {
        let lib = include_str!("lib.rs");
        let mut declared: Vec<&str> = Vec::new();
        for line in lib.lines() {
            let t = line.trim();
            let rest = t
                .strip_prefix("pub mod ")
                .or_else(|| t.strip_prefix("mod "))
                .unwrap_or("");
            if rest.is_empty() {
                continue;
            }
            let name = rest.split([';', ' ']).next().unwrap_or("");
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                declared.push(name);
            }
        }
        assert!(
            declared.len() > 10,
            "only {} module declarations parsed out of lib.rs, so the \
             derived file list is broken rather than small",
            declared.len()
        );
        let scanned: Vec<&str> = FILES.iter().map(|(n, _)| *n).collect();
        let mut missing: Vec<String> = declared
            .iter()
            .filter(|d| !scanned.iter().any(|s| *s == format!("{d}.rs")))
            .map(|d| (*d).to_string())
            .collect();
        missing.sort();
        assert!(
            missing.is_empty(),
            "lib.rs declares modules the by-value scan never reads: {:?}. Add \
             them to FILES, or the sweep cannot see a new by-value return that \
             lands in one of them.",
            missing
        );
    }

    /// The inventory, printed as a sorted table, and checked against the
    /// types' real sizes.
    ///
    /// The print is the instrument: one command
    /// (`cargo test -p ec-av1 --lib -- stack_budget -- --nocapture`)
    /// reproduces the whole table, biggest first, so "what is the largest
    /// by-value type in this crate" has an answer that is a measurement
    /// rather than a grep.
    ///
    /// The check reds on ANY change with both numbers. That is deliberate: a
    /// by-value type that grew raises the question of whether it should have
    /// been boxed instead, and auto-accepting the new number would answer
    /// that question by default.
    #[test]
    fn the_measured_inventory_is_accurate() {
        let live = measured_by_name();
        assert_eq!(
            BY_VALUE_TYPES.len(),
            live.len(),
            "the inventory names {} types but {} are measured; every row needs \
             a measured size or the sweep is a list of hopes",
            BY_VALUE_TYPES.len(),
            live.len()
        );
        let mut rows: Vec<(&str, usize)> = live.clone();
        rows.sort_by_key(|&(_, size)| std::cmp::Reverse(size));
        eprintln!(
            "\nBY-VALUE INVENTORY ({} types, budget {STACK_BUDGET} B = {} KiB)",
            rows.len(),
            STACK_BUDGET / 1024
        );
        eprintln!("{:>8}  {:>7}  {}", "BYTES", "%BUDGET", "TYPE");
        for (name, size) in &rows {
            eprintln!(
                "{size:>8}  {:>6.1}%  {name}",
                *size as f64 * 100.0 / STACK_BUDGET as f64
            );
        }
        for (name, recorded, _public, _path) in BY_VALUE_TYPES {
            let recorded = recorded.expect("every inventory row carries a measured size");
            let actual = live
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, size)| *size)
                .unwrap_or_else(|| panic!("{name} is inventoried but never measured"));
            assert_eq!(
                recorded, actual,
                "{name} is {actual} bytes, the inventory records {recorded}; \
                 update the table deliberately -- a by-value type that changed \
                 is a stack cost every caller of its constructor now pays"
            );
        }
    }

    /// The bound, asserted BY NAME. A regression in any inventoried type reds
    /// here naming the type, not "some type is too big".
    #[test]
    fn every_by_value_type_fits_the_stack_budget() {
        let live = measured_by_name();
        let mut checked = 0usize;
        for (name, _, public, path) in BY_VALUE_TYPES {
            if !public {
                continue;
            }
            checked += 1;
            let size = live
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, size)| *size)
                .unwrap_or_else(|| panic!("{name} is inventoried but never measured"));
            assert!(
                size <= STACK_BUDGET,
                "{name} is {size} bytes, over the {STACK_BUDGET}-byte by-value \
                 stack budget ({:.2}x). It is returned by value from {path}, so \
                 every caller pays it on its own stack, and the constructors \
                 nest -- one value's size is the multiplier, not the cost. \
                 Box the large field (lane-av1enchang did exactly this for \
                 Av1Encoder::dpb, and lane-av1stacksweep for Encoded's two \
                 inline CdfSnapshots) or hand back a reference.",
                size as f64 / STACK_BUDGET as f64
            );
        }
        assert!(
            checked >= 10,
            "only {checked} rows were bound-checked; the budget must apply to \
             every type a caller can be made to hold by value"
        );
        // The other half of the derivation, asserted so it cannot rot: the
        // worst a caller can owe is DEEPEST_CHAIN x the worst public return.
        let worst = live
            .iter()
            .filter(|(n, _)| {
                BY_VALUE_TYPES
                    .iter()
                    .any(|(row, _, public, _)| row == n && *public)
            })
            .map(|(_, size)| *size)
            .max()
            .expect("at least one public row");
        assert!(
            DEEPEST_PUBLIC_CHAIN * worst * 8 <= 2 * 1024 * 1024,
            "the deepest public chain ({DEEPEST_PUBLIC_CHAIN}) times the worst \
             public by-value return ({worst} B) is {} B, over an eighth of the \
             2101248-byte stack an unconfigured caller hands this crate; \
             re-derive STACK_BUDGET",
            DEEPEST_PUBLIC_CHAIN * worst
        );
    }

    /// The crate-private CDF tables stay BEHIND A BOX.
    ///
    /// The budget does not apply to `Cdfs` / `CdfSnapshot` (15,232 B each)
    /// because no `pub fn` returns them, so bounding them would calibrate the
    /// number to something no caller can reach. What keeps them off the stack
    /// is that every field holding one is a `Box`, and THIS is the pin for
    /// that -- a source scan over the four declarations.
    ///
    /// It earned its place during this lane: `Av1Encoder` was 18,168 B after
    /// lane-av1enchang boxed the DPB slot, and 15,232 of that was
    /// `carried_cdfs: Option<CdfSnapshot>` sitting inline on the very same
    /// struct. Nothing had measured it. Unboxing any of the four puts the
    /// stack cost straight back.
    #[test]
    fn the_crate_private_cdf_tables_stay_behind_a_box() {
        const PINS: &[(&str, &str, &str)] = &[
            (
                "encoder.rs",
                "DpbSlot::cdfs",
                "cdfs: Box<crate::encode::CdfSnapshot>",
            ),
            (
                "encoder.rs",
                "Av1Encoder::carried_cdfs",
                "carried_cdfs: Option<Box<crate::encode::CdfSnapshot>>",
            ),
            (
                "encode.rs",
                "Encoded::start_cdfs",
                "start_cdfs: Box<CdfSnapshot>",
            ),
            (
                "encode.rs",
                "Encoded::next_cdfs",
                "next_cdfs: Box<CdfSnapshot>",
            ),
        ];
        const FILES: &[(&str, &str)] = &[
            ("encoder.rs", include_str!("encoder.rs")),
            ("encode.rs", include_str!("encode.rs")),
        ];
        for (file, field, spelling) in PINS {
            let src = FILES
                .iter()
                .find(|(name, _)| name == file)
                .map(|(_, src)| *src)
                .unwrap_or_else(|| panic!("no source pinned for {file}"));
            let where_ = format!("{file}::{field}");
            assert!(
                src.contains(spelling),
                "{where_} is no longer declared `{spelling}`. An inline \
                 CdfSnapshot there puts {field}'s 15232 bytes back on every \
                 caller's stack -- that is how Av1Encoder was 139960 bytes \
                 before lane-av1enchang, and how it was still 18168 after it, \
                 because carried_cdfs was missed.",
            );
        }
        // And the negative control: the unboxed spellings must NOT all still
        // be present, or the scan above would be matching a comment.
        let encoder = FILES[0].1;
        assert!(
            !encoder.contains("cdfs: crate::encode::CdfSnapshot,"),
            "the box pin is matching a spelling that is not the live \
             declaration"
        );
    }

    /// CAPABILITY: really construct the heavy types on a tight stack, so a
    /// regression ABORTS THE PROCESS instead of costing invisible margin.
    ///
    /// Both arms are the deepest public chains in the crate: the encoder's
    /// three-deep `Result<Self>` constructor nest, and the four-deep
    /// `Result<Encoded>` chain, the longest by-value return path here.
    ///
    /// Measured stack need, **debug** build, 64x64, one process per point so a
    /// SIGABRT cannot truncate the sweep:
    ///
    /// | stack | before this lane boxed `Encoded` | after |
    /// |---|---|---|
    /// | 131072 | overflow | overflow |
    /// | 262144 | overflow | overflow |
    /// | 327680 | overflow | overflow |
    /// | 466944 | overflow | **overflow** |
    /// | 475136 | overflow | **ok** |
    /// | 524288 | overflow | **ok** |
    /// | 786432 | **overflow** | ok |
    /// | 1048576 | **ok** | **ok** |
    ///
    /// The gate sits at 1 MiB, now **2.2x** the deepest measured need rather
    /// than 1.33x. Boxing `Encoded`'s two inline `CdfSnapshot`s (99.2% of its
    /// 30,728 bytes) and `Av1Encoder::carried_cdfs` (15,232 of its 18,168) is
    /// what moved the threshold. The ~470 KB that remains is the encode
    /// search's own frames, which this module does not claim to shrink.
    ///
    /// Pre-fix (`CdfSnapshot` by value in the DPB) the encoder arm overflowed
    /// at 1 MiB and at 2 MiB. `EC_AV1_TIGHT_STACK_BYTES` overrides the size,
    /// which is how the table above is reproduced.
    #[test]
    fn the_deepest_by_value_constructors_fit_on_a_tight_stack() {
        use crate::encoder::{Av1Encoder, Colour, EncoderConfig, Pyramid, RateTarget};
        let stack: usize = std::env::var("EC_AV1_TIGHT_STACK_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(TIGHT_STACK);
        let config = EncoderConfig {
            width: 64,
            height: 64,
            base_q_idx: 100,
            gop: 48,
            colour: Colour::Bt709Limited,
            tile_cols_log2: 0,
            tile_rows_log2: 0,
        };
        let rate = RateTarget::Bitrate {
            bits_per_second: 1_536_000,
            frames_per_second: 24.0,
        };
        let handle = std::thread::Builder::new()
            .stack_size(stack)
            .spawn(move || {
                // Arm 1: the three-deep Result<Self> constructor nest.
                let encoder =
                    Av1Encoder::with_pyramid_and_rate_target(config, Pyramid::default(), rate)
                        .expect("with_pyramid_and_rate_target");
                let pyramid = encoder.pyramid().is_some();
                drop(encoder);
                // Arm 2: the four-deep Result<Encoded> chain, the crate's
                // largest by-value return.
                let grey = crate::encode::Picture::grey(64, 64);
                let encoded =
                    crate::encode::encode_key_frame(&grey, 100, 0.0).expect("encode_key_frame");
                (pyramid, encoded.stream.len())
            })
            .expect("spawn tight-stack thread");
        let (pyramid, bytes) = handle.join().unwrap_or_else(|_| {
            panic!(
                "a by-value constructor overflowed a {stack}-byte stack; the \
                 threshold table on this test says what that costs"
            )
        });
        assert!(pyramid, "with_pyramid_and_rate_target lost its pyramid");
        assert!(bytes > 0, "encode_key_frame produced no stream");
        eprintln!(
            "TIGHTSTACK {stack} bytes: Av1Encoder={} Encoded={} bytes, both chains returned",
            std::mem::size_of::<Av1Encoder>(),
            std::mem::size_of::<Encoded>(),
        );
    }

    /// The sweep's teeth against a NEW by-value return: re-derive the set from
    /// the crate source and require every name to be inventoried.
    ///
    /// Without this the inventory is a list of what someone remembered to
    /// measure, and a lane that adds `pub fn make() -> HugeThing` where
    /// `HugeThing` is 200 KB of inline tables passes every other test here.
    #[test]
    fn every_by_value_public_return_is_in_the_inventory() {
        let mut found: Vec<(String, String)> = Vec::new();
        for (file, src) in FILES {
            for (n, line) in src.lines().enumerate() {
                if let Some(name) = by_value_return_name(line) {
                    found.push((name, format!("{file}:{}", n + 1)));
                }
            }
        }
        found.sort();
        found.dedup();
        assert!(
            found.len() > 5,
            "the by-value return scan found {} names across {} files, which is \
             too few to be a real sweep -- the matcher, not the crate, is broken",
            found.len(),
            FILES.len()
        );

        let mut missing = Vec::new();
        for (name, site) in &found {
            if !BY_VALUE_TYPES.iter().any(|(n, _, _, _)| n == name) {
                missing.push(format!("{name} ({site})"));
            }
        }
        assert!(
            missing.is_empty(),
            "these types are returned BY VALUE from a public fn and are not in \
             the inventory: {}. Either they are thin enough not to be a stack \
             cost -- say so by adding a row with its measured size -- or they \
             are the next instance of the class this module exists for.",
            missing.join(", ")
        );
    }

    /// The control for the scan above, on synthetic text rather than by
    /// trusting the live tree to happen to contain a case.
    ///
    /// If the matcher silently stopped recognising a shape, the sweep would
    /// pass everything and this would still be asserting nothing. So: every
    /// by-value spelling the crate uses must yield its type name, and every
    /// pointer/Vec/primitive/`Self` spelling must yield nothing.
    #[test]
    fn the_inventory_scan_is_not_vacuous() {
        for synthetic in [
            "pub fn zz_a() -> ZzNotInventoried { todo!() }",
            "pub fn zz_b() -> Result<ZzNotInventoried, Error> { todo!() }",
            "pub fn zz_c() -> Result<ZzNotInventoried> { todo!() }",
            "pub fn zz_d() -> Option<ZzNotInventoried> { todo!() }",
            "pub fn zz_e() -> ZzNotInventoried<'a> { todo!() }",
        ] {
            let name = by_value_return_name(synthetic)
                .unwrap_or_else(|| panic!("the matcher does not recognise `{synthetic}`"));
            assert_eq!(
                name, "ZzNotInventoried",
                "the matcher mangled `{synthetic}` into `{name}`, so the sweep \
                 cannot see a new by-value return and its green is vacuous"
            );
            assert!(
                !BY_VALUE_TYPES.iter().any(|(n, _, _, _)| *n == name),
                "the synthetic control name is in the inventory, so the control \
                 proves nothing"
            );
        }
        for thin in [
            "pub fn zz_t() -> Box<Decoder> { todo!() }",
            "pub fn zz_u() -> Result<Vec<u8>, Error> { todo!() }",
            "pub fn zz_v() -> Option<&Path> { todo!() }",
            "pub fn zz_w() -> usize { todo!() }",
            "pub fn zz_x() -> Self { todo!() }",
            "pub fn zz_y() -> Result<Arc<Decoder>, Error> { todo!() }",
            "pub fn zz_z() { todo!() }",
            "fn zz_private() -> ZzNotInventoried { todo!() }",
        ] {
            assert_eq!(
                by_value_return_name(thin),
                None,
                "`{thin}` is a pointer, a Vec, a primitive, a `Self` the rows \
                 above already cover, or not a `pub fn` -- the matcher must not \
                 read it as an uninventoried by-value return"
            );
        }
    }

    /// The stack mapping containing `addr` in `/proc/self/task/<tid>/maps`.
    ///
    /// The returned size is a LOWER BOUND, never an exact count. Linux MERGES
    /// adjacent VMAs that share protections, and a merged region is
    /// indistinguishable from a single one in the maps file -- a thread whose
    /// 1 MiB stack abutted a neighbour's read back here as 69218304 bytes with
    /// nothing to indicate the join. Two earlier versions of this test tried to
    /// be exact anyway (asserting a byte count, then detecting the merge by
    /// looking for an abutting same-protection neighbour) and both were wrong
    /// for that reason.
    ///
    /// A lower bound is what the budget's reasoning needs: the claim is "no
    /// caller gets less than 2 MiB", and `size >= 2 MiB` is exactly that,
    /// whether the region is one mapping or three.
    fn stack_region(tid: u32, addr: usize) -> Option<(usize, usize)> {
        let maps = std::fs::read_to_string(format!("/proc/self/task/{tid}/maps")).ok()?;
        for line in maps.lines() {
            let (range, _) = line.split_once(' ')?;
            let (lo, hi) = range.split_once('-')?;
            let (lo, hi) = (
                usize::from_str_radix(lo, 16).ok()?,
                usize::from_str_radix(hi, 16).ok()?,
            );
            if addr >= lo && addr < hi {
                return Some((lo, hi));
            }
        }
        None
    }

    /// What a thread's stack ACTUALLY is, read from
    /// `/proc/self/task/<tid>/maps` rather than assumed from a default.
    ///
    /// The number the budget's reasoning rests on, measured two ways:
    ///
    /// * a thread spawned with an EXPLICIT size gets AT LEAST that size --
    ///   the control that says the parse finds stacks at all, and the shape
    ///   [`the_deepest_by_value_constructors_fit_on_a_tight_stack`] relies on;
    /// * the DEFAULT -- no `stack_size`, i.e. what a library consumer's
    ///   `std::thread::spawn` gets, and what libtest gives a test thread when
    ///   `RUST_MIN_STACK` is unset -- is at least 2 MiB.
    ///
    /// Both are floors, because [`stack_region`] can only report a lower bound.
    /// That is the right direction: the budget's claim is that no caller gets
    /// LESS than 2 MiB, and a floor is what proves it.
    ///
    /// Exact figures measured on this runtime, for the report: 2101248 bytes
    /// with the repo's `.cargo/config.toml` cap lifted, 67112960 with it
    /// applied. A library consumer gets the first, and that is the one the
    /// budget is sized for.
    #[test]
    fn test_threads_get_the_stack_they_are_given() {
        const EXPLICIT: usize = 1024 * 1024;
        let handle = std::thread::Builder::new()
            .stack_size(EXPLICIT)
            .spawn(|| {
                let tid = std::process::id();
                let marker = 0u8;
                let here = &marker as *const u8 as usize;
                let (lo, hi) =
                    stack_region(tid, here).expect("this thread's own stack is in its maps");
                (tid, lo, hi)
            })
            .expect("spawn");
        let (_tid, lo, hi) = handle.join().expect("the probe thread returned");
        let size = hi - lo;
        assert!(
            size >= EXPLICIT,
            "a thread spawned with stack_size({EXPLICIT}) has a {size}-byte \
             stack mapping; the runtime gave it less than it was promised, so \
             the maps parse is finding the wrong region"
        );
        assert!(
            size <= 512 * 1024 * 1024,
            "the mapping holding an explicitly-1-MiB thread's frame is {size} \
             bytes, which is not a stack -- the maps parse found the wrong region"
        );

        let tid = std::process::id();
        let marker = 0u8;
        let here = &marker as *const u8 as usize;
        let (lo, hi) = stack_region(tid, here).expect("this test thread's stack is in its maps");
        let size = hi - lo;
        assert!(
            size >= 2 * 1024 * 1024,
            "a default test thread's stack measured {size} bytes, under the \
             2 MiB floor the by-value budget is derived from; the smallest \
             stack a caller can hand this crate just got smaller and the bound \
             needs re-deriving"
        );
        eprintln!(
            "TESTTHREADSTACK default tid={tid} region=[{lo:x},{hi:x}) size>={size} \
             (2101248 measured exactly with the repo cap lifted, 67112960 with it)"
        );
    }
}
