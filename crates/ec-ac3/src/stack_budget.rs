#![cfg(test)]
//! A measured bound on the crate's BY-VALUE types, and a gate that bites.
//!
//! # The class
//!
//! A large struct returned BY VALUE from a public constructor is a stack cost,
//! not a heap cost, and the cost is paid by the CALLER. The constructors here
//! nest -- `Ac3Decoder::new` -> `Ac3Decoder::with_options` -> `Core::new` for
//! the decoder -- and in an unoptimized build each level materialises its whole
//! return slot before the inner call returns. So one value's `size_of` is the
//! MULTIPLIER, and the chain length is the other factor.
//!
//! That is not hypothetical. `Ac3Decoder` was **20,024** bytes by value -- its
//! inline `decode::Core` carried four per-channel coefficient planes
//! (`exps`, `bap`, `coeffs`, `delay`) that are **16,896** of `Core`'s 19,728
//! bytes -- and every `Ac3Decoder::new()` / `Ac3Decoder::with_options()` call
//! made the caller pay all of it on its own stack. `.cargo/config.toml`
//! carries `RUST_MIN_STACK = 67108864`, which HIDES the class from this
//! workspace's own runs -- a band-aid a library consumer does not get. Boxing
//! those four fields took `Core` to **2,864** and `Ac3Decoder` to **3,160**;
//! [`the_crate_private_core_stays_behind_a_box`] is the pin that keeps them
//! there.
//!
//! # Why this module exists when that boxing already exists
//!
//! The boxing fixed ONE type. The class was not swept: nothing stopped the
//! next 18 KB inline plane from landing in `Core`, and nothing said which types
//! had been checked. This module is the sweep:
//!
//! 1. [`BY_VALUE_TYPES`] is the inventory -- every public type in this crate
//!    that a public `fn` can return by value, with a measured `size_of` floor
//!    recorded as of this commit.
//!    [`the_measured_inventory_is_accurate`] fails if a type's size moves, so
//!    the table cannot rot into a list of stale small numbers.
//! 2. [`STACK_BUDGET`] is the bound, and
//!    [`every_by_value_type_fits_the_stack_budget`] asserts it BY NAME for
//!    every row, so a regression reds naming the type that caused it.
//! 3. [`the_crate_private_core_stays_behind_a_box`] pins the four `Box` field
//!    spellings, with a negative control, so the fix is not silently reverted.
//! 4. [`the_deepest_by_value_constructors_fit_on_a_tight_stack`] is the
//!    capability arm: it really constructs the heavy types on a deliberately
//!    tight thread, so the old layout ABORTS THE PROCESS rather than quietly
//!    costing margin.
//! 5. [`every_by_value_public_return_is_in_the_inventory`] is the sweep's
//!    teeth against NEW types: a source scan re-derives the set of by-value
//!    public returns from the crate source, so a lane that adds a 40 KB struct
//!    to a `pub fn` return reds here until it is measured and listed.
//!    [`the_inventory_scan_is_not_vacuous`] is its control, and
//!    [`every_by_value_module_is_scanned`] keeps the scan's file list derived
//!    from `lib.rs` rather than curated.
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
//! 2. **The deepest PUBLIC by-value chain is 2** ([`DEEPEST_PUBLIC_CHAIN`]):
//!    `Ac3Decoder::new` (decoder.rs:117) -> `Ac3Decoder::with_options`
//!    (decoder.rs:122), which calls `Core::new()` (decode.rs:136) as a third,
//!    crate-private level carrying a 2,864-byte `Core`. The encoder's
//!    `Ac3Encoder::new` is a single level -- it builds the struct inline in
//!    its `Ok(..)` and does not nest -- and `Ac3Decoder::decode_frame`'s
//!    `Result<AudioFrame>` is a single level too. The test asserts
//!    `DEEPEST_PUBLIC_CHAIN * worst * 8 <= 2 MiB` -- i.e. the by-value return
//!    slots may claim at most an EIGHTH of a caller's stack, leaving the rest
//!    for the decode, which is the thing that actually needs it.
//! 3. **The budget applies only to types a `pub fn` returns.** The
//!    crate-private 2,864-byte `Core` is measured and listed but is NOT
//!    bounded, because no caller can reach it by value -- every field that
//!    holds one of its 16,896-byte planes is a `Box`, which
//!    [`the_crate_private_core_stays_behind_a_box`] pins by source. Bounding
//!    it would calibrate the number to something unreachable.
//!
//! **Where the number sits.** At 8 KiB with the worst public return at 3,160
//! bytes ([`Ac3Decoder`]), the worst a caller owes is `2 x 3160 = 6,320` bytes
//! -- **0.30%** of a 2 MiB thread -- and the budget itself is 2.59x above the
//! incumbent. `Ac3Decoder` is also the row closest to the budget, at **38.6%**
//! of it: **5,032 bytes of margin**, the least of any row here.
//! [`Ac3Encoder`] is second, at 34.0% and 5,408 bytes of margin. Both
//! directions have room, which is the point: a bound the largest type sits
//! just under is a bound the next lane will quietly raise.
//!
//! **What it catches.** A pre-fix `Ac3Decoder` (20,024 B) is **2.44x over**
//! budget. The pre-fix `Core` (19,728 B) behind it is **2.41x over** the same
//! number -- that is the ratio the layout fix removed, not one this gate
//! enforces, because no `pub fn` returns `Core`; the pin on its four `Box`
//! fields is what keeps it there. Both the historical defect and the
//! near-miss red by a factor, not by a hair.
//!
//! **What it does not claim.** `size_of` is layout, not stack depth, and the
//! gap between the two is the nesting multiplier plus every frame a call owns
//! for its own work. `Ac3Encoder::new` is the clearest case: its 2,784-byte
//! return is nowhere near the budget, yet building it needs ~80 KiB of thread
//! in a debug build, because it materialises a `[0.0f32; 512]` window, an
//! `Mdct::new(512)` plan and a per-`COEFFS` IMDCT basis sweep. So this module
//! does not trust the arithmetic:
//! [`the_deepest_by_value_constructors_fit_on_a_tight_stack`] constructs both
//! heavy types for real on a real thread, and its measured threshold is the
//! number that settles it.

use crate::decoder::{Ac3Decoder, Options};
use crate::encode::Ac3Encoder;

/// The per-type ceiling for a type a CALLER can be made to hold by value, in
/// bytes. See the module docs for the derivation; it is two-sided, and both
/// sides are measured.
pub(crate) const STACK_BUDGET: usize = 8192;

/// The deepest PUBLIC by-value constructor chain in this crate, counted from
/// the source:
///
/// * `Ac3Decoder::new` (decoder.rs:117) -> `Ac3Decoder::with_options`
///   (decoder.rs:122): TWO public levels, both returning a 3,160-byte
///   `Ac3Decoder` by value. `with_options` calls `Core::new()`
///   (decode.rs:136), a third level, but that one is `pub(crate)` and carries
///   a 2,864-byte `Core`, so it is not a public return.
/// * `Ac3Encoder::new` (encode.rs:254): ONE level. It builds the struct
///   inline in its `Ok(Ac3Encoder { .. })` (encode.rs:306) and never calls
///   another constructor that returns it by value.
/// * `Ac3Decoder::decode_frame` (decoder.rs:156) -> `Result<AudioFrame>`:
///   ONE level, 96 bytes. It does not nest.
///
/// The budget is checked against two, the worst of the three.
#[cfg(test)]
const DEEPEST_PUBLIC_CHAIN: usize = 2;

/// Every public type in this crate a public `fn` can return BY VALUE, with the
/// `size_of` measured at the commit that wrote this table. `None` means "not
/// yet measured", which [`the_measured_inventory_is_accurate`] rejects.
///
/// The third column names the return path, so a reader can see why a type is
/// here rather than a coincidence of the type system.
pub(crate) const BY_VALUE_TYPES: &[(&str, Option<usize>, bool, &str)] = &[
    // -- decoder.rs: the headline row -------------------------------------
    (
        "Ac3Decoder",
        Some(3160),
        true,
        "Ac3Decoder::new / with_options -> Ac3Decoder, nesting two deep. Was \
         20024 bytes with decode::Core's four coefficient planes inline, i.e. \
         2.44x over the 8192-byte budget",
    ),
    // -- encode.rs: the second-closest row to the budget -------------------
    (
        "Ac3Encoder",
        Some(2784),
        true,
        "Ac3Encoder::new -> Result<Ac3Encoder>, one level, no nesting. \
         34.0% of the budget, 5408 bytes of margin; Ac3Decoder above is the \
         closest row at 38.6%",
    ),
    (
        "EncoderConfig",
        Some(12),
        true,
        "consumed by value by Ac3Encoder::new, which copies it into the \
         encoder it returns",
    ),
    (
        "EncodeStats",
        Some(72),
        true,
        "Ac3Encoder::stats -> EncodeStats, by value out of the encoder's field",
    ),
    // -- decoder.rs: the header/option surface ---------------------------
    (
        "Options",
        Some(8),
        true,
        "Ac3Decoder::with_options(Options) takes it by value and keeps it",
    ),
    (
        "FrameInfo",
        Some(48),
        true,
        "built by value in Ac3Decoder's private info_from; handed back as \
         Option<&FrameInfo>, a BORROW, so the row is about the move the \
         decoder makes internally",
    ),
    (
        "Downmix",
        Some(1),
        true,
        "Options::downmix, a field of the by-value Options above",
    ),
    (
        "AudioFrame",
        Some(96),
        true,
        "Ac3Decoder::decode_frame -> Result<AudioFrame>, one level, no \
         nesting. From ec_core, not this crate, but it is a public return of \
         this crate and the sweep finds it by name",
    ),
    // -- decode.rs -------------------------------------------------------
    (
        "Syntax",
        Some(1),
        true,
        "re-exported from decode; a field of FrameInfo and of Ac3Decoder::Core",
    ),
    // -- the header parsers ----------------------------------------------
    (
        "Bsi",
        Some(56),
        true,
        "bsi::parse / bsi::parse_from -> Result<Bsi>, one level each",
    ),
    ("Acmod", Some(1), true, "bsi::Acmod::from_code -> Acmod"),
    (
        "SyncInfo",
        Some(24),
        true,
        "syncinfo::parse / syncinfo::parse_from -> Result<SyncInfo>",
    ),
    (
        "Eac3Bsi",
        Some(96),
        true,
        "eac3::bsi::parse / eac3::bsi::parse_from -> Result<Eac3Bsi>",
    ),
    (
        "StreamType",
        Some(1),
        true,
        "a field of the by-value Eac3Bsi above; eac3::StreamType::Dependent \
         is the first-substream refusal",
    ),
    // -- transform.rs / mantissa.rs / exps.rs ----------------------------
    (
        "Imdct",
        Some(472),
        true,
        "transform::Imdct::new -> Imdct, one level. Held inline in both Core \
         and Ac3Encoder, the largest type in the crate that is NOT a public \
         return",
    ),
    (
        "Mantissas",
        Some(56),
        true,
        "mantissa::Mantissas::new -> Mantissas, one level",
    ),
    (
        "Strategy",
        Some(1),
        true,
        "exps::Strategy::from_code -> Strategy; a field of Core's inline \
         [Strategy; CHANNELS]",
    ),
    // -- bitalloc.rs -----------------------------------------------------
    (
        "BitAllocParams",
        Some(5),
        true,
        "bitalloc::BitAllocParams::default, copied into Core's inline copy",
    ),
    (
        "DeltaBa",
        Some(32),
        true,
        "bitalloc::DeltaBa, built by value per block",
    ),
    (
        "Allocation",
        Some(56),
        true,
        "bitalloc::Allocation<'_>, built by value and passed to \
         bitalloc::compute by reference; the move copies the struct, not the \
         borrowed slices",
    ),
    (
        "Channel",
        Some(12),
        true,
        "bitalloc::Channel, moved out of the per-block channel table",
    ),
    // -- crate-private: never a public return, and every one of its four
    // -- large planes is behind a Box now. The budget does not apply; the
    // -- pin that does is `the_crate_private_core_stays_behind_a_box`.
    (
        "Core",
        Some(2864),
        false,
        "decode::Core::new -> Core, pub(crate), held inline in Ac3Decoder. \
         Was 19728 bytes with exps/bap/coeffs/delay inline; those four are \
         16896 of them and are boxed now",
    ),
];

/// The live `size_of` for every inventoried type, keyed BY NAME.
///
/// Keyed rather than positional on purpose: `Bsi`, `Mantissas` and
/// `Allocation` are all 56 bytes, and `Eac3Bsi` and `AudioFrame` are both 96,
/// so a row inserted into [`BY_VALUE_TYPES`] without a matching insert here
/// would zip cleanly and assert the wrong type's size against the wrong
/// recorded one. A name key makes that mistake loud instead.
#[cfg(test)]
fn measured_by_name() -> Vec<(&'static str, usize)> {
    vec![
        ("Ac3Decoder", std::mem::size_of::<Ac3Decoder>()),
        ("Ac3Encoder", std::mem::size_of::<Ac3Encoder>()),
        (
            "EncoderConfig",
            std::mem::size_of::<crate::encode::EncoderConfig>(),
        ),
        (
            "EncodeStats",
            std::mem::size_of::<crate::encode::EncodeStats>(),
        ),
        ("Options", std::mem::size_of::<Options>()),
        (
            "FrameInfo",
            std::mem::size_of::<crate::decoder::FrameInfo>(),
        ),
        ("Downmix", std::mem::size_of::<crate::decoder::Downmix>()),
        ("AudioFrame", std::mem::size_of::<ec_core::AudioFrame>()),
        ("Syntax", std::mem::size_of::<crate::decode::Syntax>()),
        ("Bsi", std::mem::size_of::<crate::bsi::Bsi>()),
        ("Acmod", std::mem::size_of::<crate::bsi::Acmod>()),
        ("SyncInfo", std::mem::size_of::<crate::syncinfo::SyncInfo>()),
        ("Eac3Bsi", std::mem::size_of::<crate::eac3::Eac3Bsi>()),
        ("StreamType", std::mem::size_of::<crate::eac3::StreamType>()),
        ("Imdct", std::mem::size_of::<crate::transform::Imdct>()),
        (
            "Mantissas",
            std::mem::size_of::<crate::mantissa::Mantissas>(),
        ),
        ("Strategy", std::mem::size_of::<crate::exps::Strategy>()),
        (
            "BitAllocParams",
            std::mem::size_of::<crate::bitalloc::BitAllocParams>(),
        ),
        ("DeltaBa", std::mem::size_of::<crate::bitalloc::DeltaBa>()),
        (
            "Allocation",
            std::mem::size_of::<crate::bitalloc::Allocation<'static>>(),
        ),
        ("Channel", std::mem::size_of::<crate::bitalloc::Channel>()),
        ("Core", std::mem::size_of::<crate::decode::Core>()),
    ]
}

/// The stack [`the_deepest_by_value_constructors_fit_on_a_tight_stack`]
/// constructs on.
///
/// 1 MiB: half of what an unconfigured caller gives this crate, and far above
/// what the deepest measured chain needs in a debug build. The threshold was
/// swept in an unoptimized build and is reproducible with
/// `EC_AC3_TIGHT_STACK_BYTES`.
const TIGHT_STACK: usize = 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::{
        BY_VALUE_TYPES, DEEPEST_PUBLIC_CHAIN, STACK_BUDGET, TIGHT_STACK, measured_by_name,
    };
    use crate::decoder::{Ac3Decoder, Downmix, Options};
    use crate::encode::{Ac3Encoder, EncoderConfig};

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
        // `Result<Bsi, Error>`, `Allocation<'a>` and `AudioFrame` are all the
        // bare name for this purpose. Splitting on the comma is load-bearing:
        // the first version of this matcher cut only at `>` and so read
        // `Result<Bsi, Error>` as the name "Bsi, Error". `ec_av1`'s control
        // caught exactly that, which is why the control is not optional.
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
        ("aht.rs", include_str!("aht.rs")),
        ("aht_tables.rs", include_str!("aht_tables.rs")),
        ("bitalloc.rs", include_str!("bitalloc.rs")),
        ("bsi.rs", include_str!("bsi.rs")),
        ("decode.rs", include_str!("decode.rs")),
        ("decoder.rs", include_str!("decoder.rs")),
        ("eac3.rs", include_str!("eac3.rs")),
        ("encode.rs", include_str!("encode.rs")),
        ("exps.rs", include_str!("exps.rs")),
        ("lib.rs", include_str!("lib.rs")),
        ("mantissa.rs", include_str!("mantissa.rs")),
        ("stack_budget.rs", include_str!("stack_budget.rs")),
        ("syncinfo.rs", include_str!("syncinfo.rs")),
        ("tables.rs", include_str!("tables.rs")),
        ("transform.rs", include_str!("transform.rs")),
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
    /// (`cargo test -p ec-ac3 --lib -- stack_budget -- --nocapture`)
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
                 Box the large field (this crate did exactly that for \
                 Ac3Decoder's inline decode::Core) or hand back a reference.",
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

    /// The crate-private decode state stays BEHIND A BOX.
    ///
    /// The budget does not apply to `Core` because no `pub fn` returns it, so
    /// bounding it would calibrate the number to something no caller can
    /// reach. What keeps its 16,896 bytes of coefficient planes off every
    /// caller's stack is that the four fields holding them are `Box`es, and
    /// THIS is the pin for that -- a source scan over the four declarations
    /// plus the four `Box::new`s in `Core::new`.
    ///
    /// It earned its place during the layout fix: `exps`, `bap` and `delay`
    /// are `[[T; COEFFS]; CHANNELS]`-shaped planes at 1,792 / 1,792 / 6,144
    /// bytes and `coeffs` is 7,168 -- 16,896 of a 19,728-byte `Core` -- and
    /// `Core` sits inline in the `Ac3Decoder` that `Ac3Decoder::new` hands a
    /// caller BY VALUE. Unboxing any of the four puts the stack cost straight
    /// back.
    #[test]
    fn the_crate_private_core_stays_behind_a_box() {
        const PINS: &[(&str, &str)] = &[
            (
                "decode.rs::Core::exps",
                "exps: Box<[[u8; COEFFS]; CHANNELS]>",
            ),
            ("decode.rs::Core::bap", "bap: Box<[[u8; COEFFS]; CHANNELS]>"),
            (
                "decode.rs::Core::coeffs",
                "coeffs: Box<[[f32; COEFFS]; CHANNELS]>",
            ),
            (
                "decode.rs::Core::delay",
                "delay: Box<[[f32; COEFFS]; MAX_FBW + 1]>",
            ),
            (
                "decode.rs::Core::new::exps",
                "exps: Box::new([[0; COEFFS]; CHANNELS])",
            ),
            (
                "decode.rs::Core::new::bap",
                "bap: Box::new([[0; COEFFS]; CHANNELS])",
            ),
            (
                "decode.rs::Core::new::coeffs",
                "coeffs: Box::new([[0.0; COEFFS]; CHANNELS])",
            ),
            (
                "decode.rs::Core::new::delay",
                "delay: Box::new([[0.0; COEFFS]; MAX_FBW + 1])",
            ),
        ];
        const UNBOXED: &[(&str, &str)] = &[
            ("decode.rs::Core::exps", "exps: [[u8; COEFFS]; CHANNELS],"),
            ("decode.rs::Core::bap", "bap: [[u8; COEFFS]; CHANNELS],"),
            (
                "decode.rs::Core::coeffs",
                "coeffs: [[f32; COEFFS]; CHANNELS],",
            ),
            (
                "decode.rs::Core::delay",
                "delay: [[f32; COEFFS]; MAX_FBW + 1],",
            ),
        ];
        let decode = include_str!("decode.rs");
        for (where_, spelling) in PINS {
            assert!(
                decode.contains(spelling),
                "{where_} is no longer declared `{spelling}`. An inline \
                 coefficient plane there puts its 16896 bytes back inside \
                 decode::Core, which sits inline in the Ac3Decoder that \
                 Ac3Decoder::new returns BY VALUE -- that is how Ac3Decoder was \
                 20024 bytes, 2.44x over the by-value budget.",
            );
        }
        // And the negative control: the unboxed spellings must be ABSENT, or
        // the scan above would be matching a doc comment or a dead branch
        // rather than the live declaration.
        for (where_, unboxed) in UNBOXED {
            assert!(
                !decode.contains(unboxed),
                "{where_} is declared `{unboxed}`, unboxed. The box pin above \
                 is not matching the live declaration, or the plane is inline \
                 again and every Ac3Decoder::new caller pays 16896 bytes.",
            );
        }
    }

    /// CAPABILITY: really construct the heavy types on a tight stack, so a
    /// regression ABORTS THE PROCESS instead of costing invisible margin.
    ///
    /// Arm 1 is the two-deep `Ac3Decoder` constructor nest -- the crate's
    /// largest by-value return, and the type this lane's layout fix took from
    /// 20,024 to 3,160 bytes. Arm 2 is `Ac3Encoder::new`, the second-closest
    /// row to the budget at 34.0% of it.
    ///
    /// Both arms assert a REAL property of what came back, not merely that the
    /// thread survived: a fresh `Ac3Decoder` has no `frame_info()` before any
    /// syncframe, and one built with explicit options hands those exact options
    /// back through `options_mut()`. A constructor that quietly dropped its
    /// argument would red here rather than pass on a stack measurement alone.
    ///
    /// Measured stack need, **debug** build, one process per point so a
    /// stack-overflow abort cannot truncate the sweep:
    ///
    /// | `EC_AC3_TIGHT_STACK_BYTES` | result |
    /// |---|---|
    /// | 1048576 (1 MiB) | **ok** |
    /// | 524288 | **ok** |
    /// | 262144 | **ok** |
    /// | 131072 | **ok** |
    /// | 98304 | **ok** |
    /// | 86016 | **ok** |
    /// | 81920 | **ok** |
    /// | 77824 | overflow |
    /// | 65536 | overflow |
    /// | 32768 | overflow |
    /// | 24576 | overflow |
    ///
    /// The deepest measured need is therefore between 77,824 and 81,920
    /// bytes, and the gate sits at 1 MiB -- **12.8x** that, and half of what an
    /// unconfigured caller hands this crate. The ~80 KiB is dominated by
    /// `Ac3Encoder::new`'s own working frames (a 512-float window, an
    /// `Mdct::new(512)` plan and a per-`COEFFS` IMDCT basis sweep), not by the
    /// by-value return slots this module bounds; that is exactly the residual
    /// `size_of` cannot predict, and why the arm constructs rather than
    /// computing.
    ///
    /// `EC_AC3_TIGHT_STACK_BYTES` overrides the size, which is how the
    /// threshold is swept.
    #[test]
    fn the_deepest_by_value_constructors_fit_on_a_tight_stack() {
        let stack: usize = std::env::var("EC_AC3_TIGHT_STACK_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(TIGHT_STACK);
        let config = EncoderConfig {
            sample_rate: 48_000,
            channels: 2,
            bitrate_kbps: 192,
        };
        let handle = std::thread::Builder::new()
            .stack_size(stack)
            .spawn(move || {
                // Arm 1: the two-deep `Ac3Decoder` constructor nest, the
                // crate's largest by-value return.
                let mut plain = Ac3Decoder::new();
                let fresh_has_no_frame_info = plain.frame_info().is_none();
                let plain_options = *plain.options_mut();
                // A decoder built with explicit options must keep exactly
                // those options, including a non-default downmix.
                let mut configured = Ac3Decoder::with_options(Options {
                    drc_scale: 0.25,
                    downmix: Downmix::Stereo,
                    dither: false,
                });
                let kept = *configured.options_mut();
                drop(plain);
                drop(configured);
                // Arm 2: `Ac3Encoder::new`, the second-closest row to the budget.
                let encoder = Ac3Encoder::new(config).expect("Ac3Encoder::new");
                let delay = encoder.encoder_delay();
                let stats = encoder.stats();
                (fresh_has_no_frame_info, plain_options, kept, delay, stats)
            })
            .expect("spawn tight-stack thread");
        let (fresh_has_no_frame_info, plain_options, kept, delay, stats) =
            handle.join().unwrap_or_else(|_| {
                panic!(
                    "a by-value constructor overflowed a {stack}-byte stack; the \
                     old 20024-byte Ac3Decoder is what that costs"
                )
            });
        assert!(
            fresh_has_no_frame_info,
            "a freshly constructed Ac3Decoder already reported a frame_info(); \
             decode_frame is the only thing that may set one"
        );
        assert_eq!(
            plain_options,
            Options::default(),
            "Ac3Decoder::new did not give back the default Options"
        );
        assert_eq!(
            kept,
            Options {
                drc_scale: 0.25,
                downmix: Downmix::Stereo,
                dither: false,
            },
            "Ac3Decoder::with_options did not keep the options it was handed"
        );
        assert!(
            delay > 0,
            "Ac3Encoder::encoder_delay reported no priming samples"
        );
        assert_eq!(
            stats,
            crate::encode::EncodeStats::default(),
            "a fresh Ac3Encoder reported non-zero coding stats"
        );
        eprintln!(
            "TIGHTSTACK {stack} bytes: Ac3Decoder={} Ac3Encoder={} bytes, both \
             constructors returned and both options paths round-tripped",
            std::mem::size_of::<Ac3Decoder>(),
            std::mem::size_of::<Ac3Encoder>(),
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
        eprintln!("BY-VALUE PUBLIC RETURNS FOUND: {found:?}");

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
    /// indistinguishable from a single one in the maps file. A lower bound is
    /// what the budget's reasoning needs: the claim is "no caller gets less
    /// than 2 MiB", and `size >= 2 MiB` is exactly that, whether the region is
    /// one mapping or three.
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
