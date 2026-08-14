//! `ruopus` as edith consumes it, over [`ec_opus`].
//!
//! A shim, not a port: it carries the incumbent's package name and version so
//! the swap is a `[patch.crates-io]` line, and it exposes exactly the one item
//! the replica names — [`MultistreamDecoder`], built with `with_rate` and fed
//! one packet at a time with `decode_packet` (`audio.rs:1575`, `1694`;
//! `export.rs:1950`, the fidelity gate's own decoder).
//!
//! A re-export is the whole adapter, and that is a claim about two things
//! rather than about the names:
//!
//! * **Layout arguments.** `with_rate(rate, streams, coupled, &mapping)` means
//!   the same thing in both: the first `coupled` elementary streams are stereo,
//!   `mapping[c]` names the decoded channel output channel `c` reads (255 =
//!   silent), and an impossible layout panics rather than returning — which is
//!   why the replica checks the `OpusHead` it read from a file before it gets
//!   here (`audio.rs:opus_layout`).
//! * **Channel order.** Both decoders emit the *mapping's* order, which for RFC
//!   7845 family 1 is Vorbis order (FL, FC, FR, BL, BR, LFE for 5.1) and not
//!   the product's FL/FR/FC/LFE/BL/BR. The permutation stays where the
//!   incumbent's behaviour already put it, at the call site
//!   (`audio.rs:vorbis_to_film_order`); a shim that permuted here would fold
//!   surround twice.
//!
//! Errors differ in type and not in use: `ec_core::Error` where the incumbent
//! had `PacketError`, both `Display` and both consumed as `{e}` or as a
//! refused packet.

#![forbid(unsafe_code)]

pub use ec_opus::MultistreamDecoder;
