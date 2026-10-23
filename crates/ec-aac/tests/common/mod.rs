//! Helpers shared by the ec-aac test binaries that need them: `oracle`,
//! `obs_real_content`, `sbr_real_library` and the merged `it` binary.

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[allow(dead_code)] // not used by the sbr_real_library binary
pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}
