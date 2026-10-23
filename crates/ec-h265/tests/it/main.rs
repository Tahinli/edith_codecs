//! ec-h265 integration tests, one binary. Each module is one former `tests/*.rs` file;
//! the shared fixtures and reference helpers live in `common`. `perf` stays a separate
//! binary because its wall-clock assertions must not share a process with other tests.

#![forbid(unsafe_code)]

mod common;
mod conformance;
mod conformance_intra;
mod conformance_rext;
mod conformance_rext422;
mod cu_syntax;
mod dec_meta;
mod dec_shell;
mod decode;
mod dolby_vision_p8;
mod enc8_bitstream_hash;
mod encode;
mod encode_threads;
mod fuzz_regressions;
mod main10;
mod rate_control;
mod rdoq_cache_hash;
mod real_library;
mod refusals;
mod rext_generated;
mod scaling_lists;
