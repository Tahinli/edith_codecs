//! One integration-test binary for ec-vp9: every module below is a
//! former top-level `tests/*.rs` file (shared helpers live in `common` and `ivf`).

#![forbid(unsafe_code)]

mod common;
mod dump_probe;
mod fuzz_regressions;
mod hbd_exact;
mod inter_pixels_exact;
mod intraonly_exact;
mod ivf;
mod keyframe_exact;
mod libvpx_vectors;
mod no_panic;
mod odd_dimensions_exact;
mod scaledref_exact;
mod scratch_bool;
mod scratch_err;
mod scratch_hdr2;
mod scratch_hdr3;
mod scratch_hdr;
mod scratch_lfdump;
mod scratch_lossless;
mod scratch_pixdump10;
mod scratch_pixdump;
mod scratch_realsweep;
mod scratch_synth;
mod scratch_tiledbg;
mod scratch_tok;
mod subsampling_exact;
