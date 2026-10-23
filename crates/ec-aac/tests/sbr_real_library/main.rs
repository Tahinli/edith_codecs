//! The SBR chain, against real HE-AAC files pulled from the user's own media
//! directories, decoded through their real container (mp4 `esds` / Matroska
//! `CodecPrivate`) so `AacDecoder::with_config_bytes` actually arms the SBR
//! path -- `tests/oracle.rs` remuxes everything to bare ADTS first, and an
//! ADTS stream carries no AudioSpecificConfig, so its SBR chain never sees a
//! frame. Every file here is a live discovery over `~/Music`, `~/Downloads`,
//! `~/Videos`: absent files skip loudly, nothing is generated or bundled.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use ec_aac::{AacDecoder, parse_audio_specific_config};
use ec_core::{CodecId, Demuxer, Packet};
use ec_matroska::MatroskaDemuxer;
use ec_mp4::Mp4Demuxer;

/// Every `#[test]` in this file decodes real SBR content through
/// `our_decode`/`our_decode_uncapped`, and a handful of them steer that
/// decode with debug switches (`EC_AAC_SBR_HF_BYPASS`,
/// `EC_AAC_SBR_SIDEINFO_DEBUG`, `EC_AAC_SBR_NOISE_ZERO`,
/// `EC_AAC_SBR_NOISE_FRACTION`). They are set through
/// `ec_aac::knobs` guards, which override the switch on this test's thread
/// only and restore it on drop, so no test can flip another test's decode.
/// The lock below still serialises the tests because the decoder's
/// sideinfo and noise-fraction logs are process-global and read back by
/// each test.
static DECODE_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[path = "../common/mod.rs"]
mod common;
mod helpers;
mod chunk_a;
mod chunk_b;
mod chunk_c;

use helpers::*;
use chunk_a::*;
use chunk_b::*;
use chunk_c::*;
