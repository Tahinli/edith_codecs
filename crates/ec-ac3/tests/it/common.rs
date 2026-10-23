use std::process::Command;

/// Whether `ffmpeg -version` runs successfully.
pub fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}
