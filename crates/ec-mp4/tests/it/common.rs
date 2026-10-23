//! Helpers shared by the integration tests in this binary.

use ec_core::CodecId;

/// ffprobe's codec name for each of our codec ids.
pub fn ffmpeg_name(codec: CodecId) -> &'static str {
    match codec {
        CodecId::H265 => "hevc",
        CodecId::Av1 => "av1",
        CodecId::Tx3g => "mov_text",
        other => other.name(),
    }
}
