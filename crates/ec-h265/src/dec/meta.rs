//! The metadata a decoded picture carries: the HDR static metadata of the
//! coded video sequence, the VUI colour description of the SPS it was decoded
//! under, and the decoded picture hash of the picture itself.
//!
//! The SEI messages are read by `ec_h265_syntax::sei`. Mastering display and
//! content light level persist: a message applies from the access unit it arrives
//! in, through the rest of the coded video sequence, and a new IRAP picture with
//! NoRaslOutputFlag starts from nothing. The picture hash belongs to the picture
//! whose suffix SEI carries it.

use ec_h265_syntax::ColourDescription;
use ec_h265_syntax::sei::{ContentLightLevel, MasteringDisplay, PictureHash};

/// Metadata attached to one decoded picture.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PictureMeta {
    /// `mastering_display_colour_volume` in force for this picture.
    pub mastering_display: Option<MasteringDisplay>,
    /// `content_light_level_info` in force for this picture.
    pub content_light: Option<ContentLightLevel>,
    /// `preferred_transfer_characteristics` of the alternative transfer
    /// characteristics SEI in force for this picture.
    pub alternative_transfer: Option<u8>,
    /// The VUI `colour_primaries`, `transfer_characteristics` and
    /// `matrix_coeffs` of the SPS, when the VUI states them.
    pub colour_description: Option<ColourDescription>,
    /// The VUI `video_full_range_flag` of the SPS, when the VUI carries one.
    pub video_full_range_flag: Option<bool>,
    /// The `decoded_picture_hash` suffix SEI of this picture, if it came.
    pub picture_hash: Option<PictureHash>,
    /// Every slice of the picture is an I slice: the samples need no
    /// inter prediction, so they are exact with respect to the reference decoder.
    pub intra_only: bool,
}

impl PictureMeta {
    /// Take the SEI-carried fields that `other` sets, keeping the rest. Used to
    /// fold the messages of one access unit into the coded-video-sequence state.
    pub(crate) fn absorb_sei(&mut self, other: &PictureMeta) {
        if other.mastering_display.is_some() {
            self.mastering_display = other.mastering_display;
        }
        if other.content_light.is_some() {
            self.content_light = other.content_light;
        }
        if other.alternative_transfer.is_some() {
            self.alternative_transfer = other.alternative_transfer;
        }
    }
}
