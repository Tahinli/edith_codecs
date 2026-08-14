//! `oxideav-ogg` as edith consumes it, over [`ec_ogg`].
//!
//! A shim, not a port: it carries the incumbent's package name and version so
//! the swap is a `[patch.crates-io]` line, and it exposes exactly the items the
//! replica names — `mux::xiph_lace`, `mux::open_concrete`, and the concrete
//! muxer's `set_page_target_bytes` beside the `oxideav_core::Muxer` methods
//! (`export.rs:2942,2957,2961`).
//!
//! One semantic difference is owned here rather than at the call site. The
//! replica states a packet's *granule position* — where it ends — in the
//! packet's `pts`, because that is what the incumbent's muxer wrote onto the
//! page; `ec_ogg` keeps `pts` meaning presentation start and carries granule
//! positions in side data ([`ec_ogg::granule_side_data`]). The translation is
//! [`mux::OggMuxer::write_packet`], once, for every caller.

#![forbid(unsafe_code)]

use std::io::{Seek, Write};

/// A sink an Ogg file can be written to. The incumbent puts this trait in
/// `oxideav-core`; nothing in the replica names it directly (it only ever
/// passes a `Box::new(File::create(..)?)`), so it lives here until the atomic
/// oxideav swap decides where the family keeps it.
pub trait WriteSeek: Write + Seek + Send {}

impl<T: Write + Seek + Send> WriteSeek for T {}

pub mod mux;
