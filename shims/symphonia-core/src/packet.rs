//! One packet of coded bytes.

use crate::units::{Duration, Timestamp};

/// A packet as a reader hands it out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// Track this packet belongs to.
    pub track_id: u32,
    /// Presentation timestamp, in the track's own time base.
    pub pts: Timestamp,
    /// How long it plays for.
    pub dur: Duration,
    /// The coded bytes.
    pub data: Box<[u8]>,
    /// The container's own end-of-packet position, in the track's time base,
    /// when it states one — an Ogg page's granule above all, which is the
    /// only way a Vorbis (or FLAC-in-Ogg) stream's true, un-rounded length
    /// ever reaches the decoder: the codec's own bitstream never states it.
    pub granule: Option<i64>,
}

impl Packet {
    /// A packet over `data`.
    pub fn new(track_id: u32, pts: Timestamp, dur: Duration, data: &[u8]) -> Packet {
        Packet {
            track_id,
            pts,
            dur,
            data: data.to_vec().into_boxed_slice(),
            granule: None,
        }
    }

    /// The presentation timestamp.
    pub fn pts(&self) -> Timestamp {
        self.pts
    }

    /// The coded bytes.
    pub fn buf(&self) -> &[u8] {
        &self.data
    }
}
