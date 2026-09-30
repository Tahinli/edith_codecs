#![cfg(test)]
//! A measured bound on the crate's BY-VALUE types, and a gate that bites.
//!
//! # The class
//!
//! A large struct returned BY VALUE from a public constructor is a stack cost,
//! not a heap cost, and the cost is paid by the CALLER. `Encoder` is the
//! workspace's largest live instance of this class: it was **87,024 bytes** by
//! value -- ten and a half times the budget below -- because six
//! `Option<SilkEncoder>` / `Option<SilkStereoEncoder>` slots (6,648 and 19,744
//! bytes each, 79,176 together) and a 3,831-byte SILK packet scratch buffer
//! (`MAX_SILK_PACKET_BYTES = 1 + 1 + 2 * 2 + 3 * 1275`) sat INLINE on a type
//! `Encoder::new` hands back by value. `Decoder` was 7,576, 92.5% of the
//! budget, with a 6,304-byte `SilkDecoder` inline.
//!
//! Both are boxed now (82,952 bytes moved off the stack onto the heap: six
//! `Box`ed SILK encoders, `silk_buf: Box<[u8; MAX_SILK_PACKET_BYTES]>`, and
//! `Decoder::silk: Box<SilkDecoder>`), and this module is what stops the
//! numbers from walking back.
//!
//! # Why this module exists
//!
//! 1. [`BY_VALUE_TYPES`] is the inventory -- every type in this crate a public
//!    `fn` can return or take by value, with a measured `size_of` recorded as
//!    of this commit. [`the_measured_inventory_is_accurate`] fails if a size
//!    moves, so the table cannot rot into a list of stale small numbers.
//! 2. [`STACK_BUDGET`] is the bound, and
//!    [`every_by_value_type_fits_the_stack_budget`] asserts it BY NAME for
//!    every budgeted row, so a regression reds naming the type that caused it.
//! 3. [`the_crate_private_silk_encoders_stay_behind_a_box`] pins the eight
//!    `Box` spellings that moved the 87 KB off the stack, with a negative
//!    control so the scan cannot be matching a doc comment.
//! 4. [`the_deepest_by_value_constructors_fit_on_a_tight_stack`] is the
//!    capability arm: it really constructs `Encoder::new` and `Decoder::new` on
//!    a deliberately tight thread, so the old layout ABORTS THE PROCESS rather
//!    than quietly costing margin. Red/green in milliseconds.
//! 5. [`every_by_value_public_return_is_in_the_inventory`] is the sweep's
//!    teeth against NEW types: a source scan re-derives the set of by-value
//!    public returns from the crate source, so a lane that adds a 40 KB struct
//!    to a `pub fn` return reds here until it is measured and listed.
//!    [`the_inventory_scan_is_not_vacuous`] is its control, and
//!    [`every_by_value_module_is_scanned`] keeps the scan's file list from
//!    falling behind a new `mod` in `lib.rs`.
//!
//! # The bound, and why this number
//!
//! [`STACK_BUDGET`] = 8192 bytes (8 KiB), the same number ec-av1's identical
//! sweep uses, and it is bounded from BOTH sides by measurement:
//!
//! * **The smallest stack a caller can hand this crate is 2 MiB** -- libtest's
//!   default test thread, and what `std::thread::Builder::new()` gives a
//!   caller who does not ask for a size. This workspace's
//!   `.cargo/config.toml` carries `RUST_MIN_STACK = 67108864`, which hides
//!   the problem from its own runs; a library consumer gets no
//!   `.cargo/config.toml` and no such cover.
//! * **The deepest PUBLIC by-value chain is 2**
//!   ([`DEEPEST_PUBLIC_CHAIN`]): `MultistreamDecoder::with_rate` ->
//!   `MultistreamDecoder::try_with_rate` (`multistream.rs:47` -> `:63`), both
//!   handing back a `MultistreamDecoder` by value, so both frames hold a
//!   return slot at once. `Encoder::new` (`encoder.rs:166`) and `Decoder::new`
//!   (`lib.rs:136`) are ONE level each -- neither delegates to an inner
//!   constructor of its own type, they build the struct literal inline -- so
//!   the worst live set of by-value return slots is two frames, and the test
//!   asserts `DEEPEST_PUBLIC_CHAIN * worst * 8 <= 2 MiB`.
//! * **The budget applies to types a public `fn` returns by value.** The
//!   `Box`ed payloads are listed with `public: false` and measured, but not
//!   budgeted, because the budget's job is to bound what a CALLER can be made
//!   to own and none of them is reachable that way any more -- every field
//!   that holds one is a `Box`, which
//!   [`the_crate_private_silk_encoders_stay_behind_a_box`] pins by source.
//!
//! **Where the number sits.** At 8 KiB with the worst budgeted return at 4,072
//! bytes ([`Encoder`]), the worst a caller owes is `2 x 4072 = 8,144` bytes --
//! **0.39%** of a 2 MiB thread -- and the budget sits 2.01x above the
//! incumbent. The pre-fix `Encoder` (87,024 B) is **10.62x over** and the
//! pre-fix `Decoder` (7,576 B) sat at 92.5% of it, so the historical defect
//! and the near-miss both red by a factor rather than by a hair.
//!
//! **What it does not claim.** `size_of` is layout, not stack depth: the
//! measured stack/size ratio on a by-value constructor here is several times
//! over, because the constructor's own frames add on top of the return slot.
//! That residual is why
//! [`the_deepest_by_value_constructors_fit_on_a_tight_stack`] constructs for
//! real instead of trusting the arithmetic.

use crate::Decoder;
use crate::encoder::Encoder;

/// The per-type ceiling for a type a CALLER can be made to hold by value, in
/// bytes. See the module docs for the derivation; it is two-sided, and both
/// sides are measured.
pub(crate) const STACK_BUDGET: usize = 8192;

/// The deepest PUBLIC by-value constructor chain in this crate, counted from
/// the source: `MultistreamDecoder::with_rate` -> `MultistreamDecoder::
/// try_with_rate` is two, both returning the same type by value so both frames
/// hold a `MultistreamDecoder` return slot at once. `Encoder::new` and
/// `Decoder::new` are one level each -- neither delegates to an inner
/// constructor of its own type. The budget is checked against two, the worse
/// of the chains.
const DEEPEST_PUBLIC_CHAIN: usize = 2;

/// Every type in this crate a public `fn` can hand back BY VALUE (or take by
/// value in a public signature), with the `size_of` measured at the commit
/// that wrote this table. `None` means "not yet measured", which
/// [`the_measured_inventory_is_accurate`] rejects.
///
/// The third column names the return path, so a reader can see why a type is
/// here rather than a coincidence of the type system. `false` in that column
/// means the type is NOT budgeted: it is a `Box`ed payload, reachable only
/// through a pointer now, and the pin that keeps it that way is
/// [`the_crate_private_silk_encoders_stay_behind_a_box`].
pub(crate) const BY_VALUE_TYPES: &[(&str, Option<usize>, bool, &str)] = &[
    // -- lib.rs -----------------------------------------------------------
    (
        "Decoder",
        Some(1280),
        true,
        "Decoder::new -> Result<Decoder>. Was 7576, 92.5% of the budget, \
         because a 6304-byte SilkDecoder sat inline; that field is now a Box",
    ),
    // -- encoder.rs -------------------------------------------------------
    (
        "Encoder",
        Some(4072),
        true,
        "Encoder::new -> Result<Encoder>. Was 87024, 10.62x the budget: six \
         inline Option<SilkEncoder>/Option<SilkStereoEncoder> slots (79176 \
         together) and an inline 3831-byte silk_buf",
    ),
    (
        "Application",
        Some(1),
        true,
        "Encoder::new(.., application: Application) takes it by value and \
         stores it inline in every Encoder",
    ),
    // -- celt.rs ----------------------------------------------------------
    (
        "CeltDecoder",
        Some(1168),
        true,
        "celt::CeltDecoder::new(channels, downsample) -> Self; also moved by \
         value into Decoder::celt",
    ),
    // -- celt_enc.rs ------------------------------------------------------
    (
        "CeltEncoder",
        Some(1592),
        false,
        "celt_enc::CeltEncoder::new -> Self. `pub`, and stored inline in \
         Encoder::celt, but 1592 is 19% of budget and it is the second \
         largest field of a type that is itself budgeted; the crate-private \
         19744 SilkStereoEncoder is the one this module had to pin",
    ),
    (
        "CeltFrameDiag",
        Some(424),
        true,
        "celt_enc::last_diag(&self) -> &CeltFrameDiag hands back a \
         reference; the row is here because Encoder::last_celt_diag and \
         Decoder::last_celt_diag name it in their signatures",
    ),
    // -- multistream.rs / multistream_enc.rs ------------------------------
    (
        "MultistreamDecoder",
        Some(96),
        true,
        "MultistreamDecoder::with_rate / try_with_rate, the crate's deepest \
         public by-value chain: with_rate -> try_with_rate -> Result<..>",
    ),
    (
        "MultistreamEncoder",
        Some(88),
        true,
        "MultistreamEncoder::surround_5_1 -> Result<MultistreamEncoder>",
    ),
    // -- packet.rs --------------------------------------------------------
    (
        "Packet",
        Some(40),
        true,
        "Packet::parse -> Result<Packet<'a>>",
    ),
    ("Toc", Some(3), true, "Toc::new(byte: u8) -> Toc"),
    (
        "Mode",
        Some(1),
        true,
        "Toc::mode(self) -> Mode; also stored inline in Decoder::prev_mode",
    ),
    (
        "Bandwidth",
        Some(1),
        true,
        "Toc::bandwidth(self) -> Bandwidth; also stored inline in \
         Encoder::bandwidth",
    ),
    // -- range.rs ---------------------------------------------------------
    (
        "RangeEncoder",
        Some(80),
        true,
        "RangeEncoder::new() -> Self; stored inline in Encoder::range",
    ),
    (
        "RangeDecoder",
        Some(56),
        true,
        "RangeDecoder::new(buf) -> RangeDecoder<'a>",
    ),
    (
        "EncSnapshot",
        Some(48),
        true,
        "RangeEncoder::snapshot(&self) -> EncSnapshot",
    ),
    // -- silk.rs ----------------------------------------------------------
    (
        "SilkDecoder",
        Some(6304),
        true,
        "silk::SilkDecoder::new -> Self, and it is `pub use`d at the crate \
         root, so 6304 bytes is a caller's by-value cost TODAY even though \
         Decoder::new moves it straight into a Box. At 77% of the budget it \
         is the LARGEST budgeted row in the table, and the one that says how \
         little headroom the budget would have left if it had stayed inline",
    ),
    (
        "SilkDecIndices",
        Some(104),
        true,
        "SilkDecoder::last_indices(&self) -> SilkDecIndices",
    ),
    // -- silk_enc_write.rs ------------------------------------------------
    (
        "SilkEncoder",
        Some(6648),
        false,
        "silk_enc_write::SilkEncoder::new -> Self. `pub`, `pub use`d at the \
         crate root, and returned by value -- but every field that holds one \
         is an Option<Box<..>>, so no CALLER can be made to own it",
    ),
    (
        "SilkStereoEncoder",
        Some(19744),
        false,
        "silk_enc_write::SilkStereoEncoder::new -> Self. The largest single \
         type in the crate at 2.41x the budget, and the reason three of the \
         six Encoder slots are boxed rather than one",
    ),
    (
        "SilkFrameDiag",
        Some(56),
        true,
        "SilkEncoder::last_diag(&self) -> &SilkFrameDiag hands back a \
         reference",
    ),
    // -- analysis.rs (crate-private) --------------------------------------
    (
        "TonalityAnalysis",
        Some(2168),
        false,
        "analysis::TonalityAnalysis::new(sample_rate) -> Self, stored \
         INLINE in Encoder::analysis. 2168 is inside the budget and the field \
         is reached only through an Encoder that is itself budgeted, so it is \
         measured here rather than pinned behind a Box",
    ),
    (
        "AnalysisInfo",
        Some(60),
        false,
        "analysis::AnalysisInfo::default() -> Self, stored inline in \
         Encoder::info",
    ),
];

/// The live `size_of` for every inventoried type, keyed BY NAME.
///
/// Keyed rather than positional on purpose: `Mode`, `Bandwidth` and
/// `Application` are all 1 byte, and a row inserted into [`BY_VALUE_TYPES`]
/// without a matching insert here would zip cleanly and assert the wrong
/// type's size against the wrong recorded one. A name key makes that mistake
/// loud instead.
fn measured_by_name() -> Vec<(&'static str, usize)> {
    vec![
        ("Decoder", std::mem::size_of::<Decoder>()),
        ("Encoder", std::mem::size_of::<Encoder>()),
        (
            "Application",
            std::mem::size_of::<crate::encoder::Application>(),
        ),
        (
            "CeltDecoder",
            std::mem::size_of::<crate::celt::CeltDecoder>(),
        ),
        (
            "CeltEncoder",
            std::mem::size_of::<crate::celt_enc::CeltEncoder>(),
        ),
        (
            "CeltFrameDiag",
            std::mem::size_of::<crate::celt_enc::CeltFrameDiag>(),
        ),
        (
            "MultistreamDecoder",
            std::mem::size_of::<crate::multistream::MultistreamDecoder>(),
        ),
        (
            "MultistreamEncoder",
            std::mem::size_of::<crate::multistream_enc::MultistreamEncoder>(),
        ),
        (
            "Packet",
            std::mem::size_of::<crate::packet::Packet<'static>>(),
        ),
        ("Toc", std::mem::size_of::<crate::packet::Toc>()),
        ("Mode", std::mem::size_of::<crate::packet::Mode>()),
        ("Bandwidth", std::mem::size_of::<crate::packet::Bandwidth>()),
        (
            "RangeEncoder",
            std::mem::size_of::<crate::range::RangeEncoder>(),
        ),
        (
            "RangeDecoder",
            std::mem::size_of::<crate::range::RangeDecoder<'static>>(),
        ),
        (
            "EncSnapshot",
            std::mem::size_of::<crate::range::EncSnapshot>(),
        ),
        (
            "SilkDecoder",
            std::mem::size_of::<crate::silk::SilkDecoder>(),
        ),
        (
            "SilkDecIndices",
            std::mem::size_of::<crate::silk::SilkDecIndices>(),
        ),
        (
            "SilkEncoder",
            std::mem::size_of::<crate::silk_enc_write::SilkEncoder>(),
        ),
        (
            "SilkStereoEncoder",
            std::mem::size_of::<crate::silk_enc_write::SilkStereoEncoder>(),
        ),
        (
            "SilkFrameDiag",
            std::mem::size_of::<crate::silk_enc_write::SilkFrameDiag>(),
        ),
        (
            "TonalityAnalysis",
            std::mem::size_of::<crate::analysis::TonalityAnalysis>(),
        ),
        (
            "AnalysisInfo",
            std::mem::size_of::<crate::analysis::AnalysisInfo>(),
        ),
    ]
}

/// The stack [`the_deepest_by_value_constructors_fit_on_a_tight_stack`]
/// constructs on.
///
/// 256 KiB: an eighth of what an unconfigured caller gives this crate, and
/// **5.3x** the deepest need measured in a debug build on the current layout
/// (the constructors complete at 48 KiB and abort at 32 KiB) while sitting
/// **2.1x below** the pre-fix layout's own threshold, which completes at
/// 544 KiB and aborts at 528 KiB. That gap is what makes this a capability
/// arm rather than a formality: at 256 KiB the layout this crate used to
/// ship ABORTS THE PROCESS. The sweep is reproducible with
/// `EC_OPUS_TIGHT_STACK_BYTES`.
const TIGHT_STACK: usize = 256 * 1024;

#[cfg(test)]
mod tests {
    use super::{
        BY_VALUE_TYPES, DEEPEST_PUBLIC_CHAIN, STACK_BUDGET, TIGHT_STACK, measured_by_name,
    };
    use crate::Decoder;
    use crate::encoder::Encoder;

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
        // `Result<Decoder, Error>`, `RangeDecoder<'a>` and `Packet<'a>` are
        // all the bare name for this purpose. Splitting on the comma is
        // load-bearing: a matcher that cut only at `>` would read
        // `Result<Decoder, Error>` as the name "Decoder, Error".
        // `the_inventory_scan_is_not_vacuous` covers exactly that, which is
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

    /// The source files the by-value scan reads. Every module `lib.rs`
    /// declares, plus `lib.rs` itself (which is where `Decoder` and
    /// `Encoder::new`'s crate-root sibling live) and this module.
    const FILES: &[(&str, &str)] = &[
        ("analysis.rs", include_str!("analysis.rs")),
        ("celt.rs", include_str!("celt.rs")),
        ("celt_enc.rs", include_str!("celt_enc.rs")),
        ("encoder.rs", include_str!("encoder.rs")),
        ("lib.rs", include_str!("lib.rs")),
        ("multistream.rs", include_str!("multistream.rs")),
        ("multistream_enc.rs", include_str!("multistream_enc.rs")),
        ("ogg.rs", include_str!("ogg.rs")),
        ("packet.rs", include_str!("packet.rs")),
        ("range.rs", include_str!("range.rs")),
        ("silk.rs", include_str!("silk.rs")),
        ("silk_enc.rs", include_str!("silk_enc.rs")),
        ("silk_enc_write.rs", include_str!("silk_enc_write.rs")),
        ("stack_budget.rs", include_str!("stack_budget.rs")),
    ];

    /// Every module `lib.rs` declares, so the scan's file list cannot silently
    /// fall behind a new module. A new `mod foo;` with a by-value `pub fn`
    /// that the scan never reads is exactly the hole this class would come
    /// back through, so the list is derived, not curated.
    ///
    /// `pub(crate) mod foo;` is a module too, and ec-opus has two of them
    /// (`analysis`, `silk_enc`), so the derivation strips the visibility
    /// parenthesised form as well as the bare `pub`.
    #[test]
    fn every_by_value_module_is_scanned() {
        let lib = include_str!("lib.rs");
        let mut declared: Vec<&str> = Vec::new();
        for line in lib.lines() {
            let t = line.trim();
            let after_pub = if let Some(rest) = t.strip_prefix("pub(") {
                // `pub(crate) mod foo;` / `pub(super) mod foo;`
                rest.split_once(')')
                    .map(|(_, after)| after.trim_start())
                    .unwrap_or("")
            } else {
                t.strip_prefix("pub ").unwrap_or(t)
            };
            let Some(rest) = after_pub.strip_prefix("mod ") else {
                continue;
            };
            let name = rest.split([';', ' ']).next().unwrap_or("");
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                declared.push(name);
            }
        }
        assert!(
            declared.len() > 8,
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
    /// The print is the instrument: one command reproduces the whole table,
    /// biggest first, so "what is the largest by-value type in this crate"
    /// has an answer that is a measurement rather than a grep.
    ///
    /// `cargo test -p ec-opus --lib -- stack_budget -- --nocapture` is the
    /// command, with one wrinkle in THIS repo: `.cargo/config.toml` wraps
    /// every test binary in `scripts/memguard-runner.sh`, which buffers a
    /// green run's stdout and prints nothing. The table appears on a failing
    /// run, or by invoking the built binary directly
    /// (`target/debug/deps/ec_opus-* stack_budget --nocapture`).
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
                 Box the large field: Encoder's six SILK slots and silk_buf, \
                 and Decoder::silk, are Boxed for exactly this reason and \
                 took Encoder from 87024 bytes to this one.",
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
             2 MiB stack an unconfigured caller hands this crate; re-derive \
             STACK_BUDGET",
            DEEPEST_PUBLIC_CHAIN * worst
        );
    }

    /// The SILK encoders and the SILK packet buffer stay BEHIND A BOX.
    ///
    /// The budget does not apply to `SilkEncoder` (6,648 B),
    /// `SilkStereoEncoder` (19,744 B) or the 3,831-byte `silk_buf`, because
    /// every field holding one is a `Box` and no caller can reach them by
    /// value any more. What keeps them off the stack is those exact `Box`
    /// spellings, and THIS is the pin for that -- a source scan over the
    /// declarations, with a negative control so it cannot be matching a doc
    /// comment.
    ///
    /// It earned its place, measured: unboxing `silk_buf` takes `Encoder` from
    /// 4,072 to **7,896** bytes and reds this pin naming
    /// `encoder.rs::Encoder::silk_buf` and
    /// [`the_measured_inventory_is_accurate`] naming `Encoder`. Unboxing any
    /// of the six SILK slots is far larger -- `Encoder` was 87,024 bytes
    /// before the boxing, 10.62x the budget.
    #[test]
    fn the_crate_private_silk_encoders_stay_behind_a_box() {
        const PINS: &[(&str, &str, &str)] = &[
            (
                "encoder.rs",
                "Encoder::silk_nb",
                "silk_nb: Option<Box<SilkEncoder>>",
            ),
            (
                "encoder.rs",
                "Encoder::silk_mb",
                "silk_mb: Option<Box<SilkEncoder>>",
            ),
            (
                "encoder.rs",
                "Encoder::silk_wb",
                "silk_wb: Option<Box<SilkEncoder>>",
            ),
            (
                "encoder.rs",
                "Encoder::silk_stereo_nb",
                "silk_stereo_nb: Option<Box<SilkStereoEncoder>>",
            ),
            (
                "encoder.rs",
                "Encoder::silk_stereo_mb",
                "silk_stereo_mb: Option<Box<SilkStereoEncoder>>",
            ),
            (
                "encoder.rs",
                "Encoder::silk_stereo_wb",
                "silk_stereo_wb: Option<Box<SilkStereoEncoder>>",
            ),
            (
                "encoder.rs",
                "Encoder::silk_buf",
                "silk_buf: Box<[u8; MAX_SILK_PACKET_BYTES]>",
            ),
            ("lib.rs", "Decoder::silk", "silk: Box<SilkDecoder>"),
        ];
        const SCANNED: &[(&str, &str)] = &[
            ("encoder.rs", include_str!("encoder.rs")),
            ("lib.rs", include_str!("lib.rs")),
        ];
        for (file, field, spelling) in PINS {
            let src = SCANNED
                .iter()
                .find(|(name, _)| name == file)
                .map(|(_, src)| *src)
                .unwrap_or_else(|| panic!("no source pinned for {file}"));
            let where_ = format!("{file}::{field}");
            assert!(
                src.contains(spelling),
                "{where_} is no longer declared `{spelling}`. An inline SILK \
                 encoder or scratch buffer there puts {field}'s bytes back on \
                 every caller's stack -- that is how Encoder was 87024 bytes \
                 and Decoder 7576 before the boxing, 10.62x the by-value budget \
                 on the type Encoder::new hands back by value."
            );
        }
        // And the negative control: the UNBOXED spellings must be ABSENT, or
        // the scan above could be matching a doc comment instead of the live
        // declaration. The unboxed forms are what this lane removed, so their
        // absence is the proof the pin is reading the code.
        const UNBOXED: &[(&str, &str, &str)] = &[
            (
                "encoder.rs",
                "Encoder::silk_nb",
                "silk_nb: Option<SilkEncoder>,",
            ),
            (
                "encoder.rs",
                "Encoder::silk_stereo_wb",
                "silk_stereo_wb: Option<SilkStereoEncoder>,",
            ),
            (
                "encoder.rs",
                "Encoder::silk_buf",
                "silk_buf: [u8; MAX_SILK_PACKET_BYTES],",
            ),
            ("lib.rs", "Decoder::silk", "silk: SilkDecoder,"),
        ];
        for (file, field, spelling) in UNBOXED {
            let src = SCANNED
                .iter()
                .find(|(name, _)| name == file)
                .map(|(_, src)| *src)
                .unwrap_or_else(|| panic!("no source pinned for {file}"));
            assert!(
                !src.contains(spelling),
                "{} is declared unboxed as `{spelling}`. The box pin is \
                 matching a spelling that is not the live declaration, and \
                 {field}'s bytes are back on every caller's stack.",
                format!("{file}::{field}")
            );
        }
    }

    /// CAPABILITY: really construct the heavy types on a tight stack, so a
    /// regression ABORTS THE PROCESS instead of costing invisible margin.
    ///
    /// Both arms are the crate's by-value constructors: `Encoder::new`, the
    /// type that was 87,024 bytes, and `Decoder::new`, 7,576 -- plus a real
    /// 20 ms SILK packet decoded through the `Decoder` that arm built, so the
    /// decoder is proved live on this thread rather than merely constructed.
    ///
    /// Measured stack need, **debug** build, one process per point so a
    /// SIGABRT cannot truncate the sweep:
    ///
    /// | stack | pre-fix layout (inline SILK slots) | current layout |
    /// |---|---|---|
    /// | 32768 | overflow | **overflow** |
    /// | 49152 | overflow | **ok** |
    /// | 131072 | overflow | ok |
    /// | 262144 (**the gate**) | **overflow** | **ok** |
    /// | 524288 | **overflow** | ok |
    /// | 557056 | ok | ok |
    /// | 786432 | ok | ok |
    /// | 1048576 | ok | ok |
    ///
    /// The gate sits at 256 KiB, **5.3x** the measured need of the current
    /// layout and **2.1x** below the pre-fix layout's own threshold. Note
    /// what the upper rows say: the pre-fix layout SURVIVED 1 MiB and 2 MiB,
    /// so a gate set at 1 MiB would have been a formality -- a test that
    /// passes for the old layout proves nothing. `EC_OPUS_TIGHT_STACK_BYTES`
    /// overrides the size, which is how the table is reproduced.
    ///
    /// The results are checked, not just the absence of a crash: an `Encoder`
    /// that constructed but came back with the wrong channel count, or a
    /// `Decoder` that decoded a packet to silence, would both pass a
    /// "did not overflow" assertion.
    #[test]
    fn the_deepest_by_value_constructors_fit_on_a_tight_stack() {
        use crate::encoder::Application;
        let stack: usize = std::env::var("EC_OPUS_TIGHT_STACK_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(TIGHT_STACK);
        let handle = std::thread::Builder::new()
            .stack_size(stack)
            .spawn(move || {
                // Arm 1: the by-value constructor that was 10.62x the budget.
                let encoder = Encoder::new(48000, 1, Application::Audio)
                    .expect("Encoder::new(48000, 1, Audio)");
                let channels = encoder.channels();
                let rate = encoder.sample_rate();
                drop(encoder);
                let mut decoder = Decoder::new(48000, 1).expect("Decoder::new(48000, 1)");
                // A real packet, so the decoder is proved live on this thread
                // rather than merely constructed: the SILK-only 20 ms packet
                // below decodes through the same by-value Decoder.
                let mut out = vec![0.0f32; 5760];
                let written = decoder
                    .decode_float(SILK_ONLY_PACKET, &mut out)
                    .expect("decode_float on the tight-stack thread");
                let peak = out[..written].iter().fold(0.0f32, |m, s| m.max(s.abs()));
                (channels, rate, written, peak)
            })
            .expect("spawn tight-stack thread");
        let (channels, rate, pcm_len, peak) = handle.join().unwrap_or_else(|_| {
            panic!(
                "a by-value constructor overflowed a {stack}-byte stack; \
                 EC_OPUS_TIGHT_STACK_BYTES reproduces the threshold sweep"
            )
        });
        assert_eq!(channels, 1, "Encoder::new lost its channel count");
        assert_eq!(rate, 48000, "Encoder::new lost its sample rate");
        assert_eq!(
            pcm_len, 960,
            "Decoder::decode_float returned {pcm_len} samples, not the 960 one \
             20 ms mono frame at 48 kHz holds; the tight-stack thread built a \
             Decoder that cannot decode"
        );
        assert!(
            peak > 0.0,
            "Decoder::decode_float returned silence (peak {peak}) on the \
             tight-stack thread"
        );
        eprintln!(
            "TIGHTSTACK {stack} bytes: Encoder={} Decoder={} SilkDecoder={} \
             bytes, both constructors returned and decoded 960 samples",
            std::mem::size_of::<Encoder>(),
            std::mem::size_of::<Decoder>(),
            std::mem::size_of::<crate::silk::SilkDecoder>(),
        );
    }

    /// A SILK-only 20 ms Opus packet: TOC byte `0x48` (config 9 = SILK
    /// wideband 20 ms, stereo bit 0, code 0 = SILK only) followed by a frame
    /// count byte `0x00` (one frame) and a 100-byte payload. Config 9 is
    /// SILK-only, so decoding it exercises `SilkDecoder`'s state rather than
    /// CELT's, and the crate decodes it to exactly 960 samples at 48 kHz.
    const SILK_ONLY_PACKET: &[u8] = &[
        0x48, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

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
}
