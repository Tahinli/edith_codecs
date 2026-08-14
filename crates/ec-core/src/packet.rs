//! Compressed data units and the shared cheap-clone byte buffer.

use std::ops::{Bound, Deref, RangeBounds};
use std::sync::Arc;

use crate::timebase::TimeBase;

/// Reference-counted byte range: clone is a refcount bump, `slice` is free.
///
/// This is what keeps demuxed packets and decoded planes zero-copy — a
/// container reads one cluster into a `Vec<u8>`, wraps it once, and hands out
/// packets that borrow into it without copying or borrowing a lifetime (so the
/// result stays `Send + 'static`).
#[derive(Clone, Default)]
pub struct Buf {
    data: Arc<[u8]>,
    off: usize,
    len: usize,
}

impl Buf {
    /// Empty buffer, no allocation beyond the empty `Arc`.
    pub fn new() -> Buf {
        Buf::default()
    }

    /// Take ownership of a `Vec` without copying its bytes.
    pub fn from_vec(v: Vec<u8>) -> Buf {
        let len = v.len();
        Buf {
            data: Arc::from(v),
            off: 0,
            len,
        }
    }

    /// Copy a slice into a new buffer.
    pub fn copy_from_slice(s: &[u8]) -> Buf {
        Buf::from_vec(s.to_vec())
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when the buffer holds no bytes.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// A sub-range of this buffer, sharing the same allocation.
    ///
    /// Panics on an out-of-bounds range, exactly like slicing a `&[u8]`.
    pub fn slice(&self, range: impl RangeBounds<usize>) -> Buf {
        let start = match range.start_bound() {
            Bound::Included(&n) => n,
            Bound::Excluded(&n) => n + 1,
            Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            Bound::Included(&n) => n + 1,
            Bound::Excluded(&n) => n,
            Bound::Unbounded => self.len,
        };
        assert!(start <= end && end <= self.len, "Buf::slice out of bounds");
        Buf {
            data: Arc::clone(&self.data),
            off: self.off + start,
            len: end - start,
        }
    }
}

impl Deref for Buf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.data[self.off..self.off + self.len]
    }
}

impl AsRef<[u8]> for Buf {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl From<Vec<u8>> for Buf {
    fn from(v: Vec<u8>) -> Buf {
        Buf::from_vec(v)
    }
}

impl From<&[u8]> for Buf {
    fn from(s: &[u8]) -> Buf {
        Buf::copy_from_slice(s)
    }
}

impl PartialEq for Buf {
    fn eq(&self, other: &Buf) -> bool {
        **self == **other
    }
}

impl Eq for Buf {}

impl std::fmt::Debug for Buf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Buf({} bytes)", self.len)
    }
}

/// Out-of-band data attached to a packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SideData {
    /// Codec configuration in the codec's own form (avcC, hvcC, av1C,
    /// AudioSpecificConfig, ...) when it arrives in band rather than in
    /// [`crate::registry::CodecParameters::extradata`].
    CodecConfig(Buf),
    /// Palette for palettised subtitle/image codecs.
    Palette(Buf),
    /// ISO 14496-12 3x3 display matrix (rotation/flip).
    DisplayMatrix(Buf),
    /// Anything a single container needs and nothing else understands.
    Custom {
        /// Container-defined discriminator.
        kind: u32,
        /// Raw payload.
        data: Buf,
    },
}

/// Per-packet booleans. All default to false.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PacketFlags {
    /// Decoding may start here (IDR, audio sync frame, subtitle cue start).
    pub keyframe: bool,
    /// Carries headers/config only, no presentable sample.
    pub header: bool,
    /// Known-damaged payload; decoders may attempt it and must not panic.
    pub corrupt: bool,
    /// Decode for state but do not present (post-seek pre-roll).
    pub discard: bool,
}

/// One compressed access unit of one stream.
///
/// Fields are public: containers and codecs assemble packets field by field,
/// and a builder chain would only hide that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// Index of the stream in [`crate::registry::Demuxer::streams`].
    pub stream: u32,
    /// Base of `pts`, `dts` and `duration`.
    pub time_base: TimeBase,
    /// Presentation timestamp, when the container states one.
    pub pts: Option<i64>,
    /// Decode timestamp, when it differs from `pts` (B-frames).
    pub dts: Option<i64>,
    /// Duration in `time_base` ticks, when known.
    pub duration: Option<i64>,
    /// Per-packet flags.
    pub flags: PacketFlags,
    /// Out-of-band extras; empty for almost every packet.
    pub side_data: Vec<SideData>,
    /// Compressed payload.
    pub data: Buf,
}

impl Packet {
    /// A packet with no timestamps and no flags set.
    pub fn new(stream: u32, time_base: TimeBase, data: impl Into<Buf>) -> Packet {
        Packet {
            stream,
            time_base,
            pts: None,
            dts: None,
            duration: None,
            flags: PacketFlags::default(),
            side_data: Vec::new(),
            data: data.into(),
        }
    }

    /// Set the presentation timestamp, for the callers that build a packet in
    /// one expression.
    pub fn with_pts(mut self, pts: i64) -> Packet {
        self.pts = Some(pts);
        self
    }

    /// Set the duration in `time_base` ticks.
    pub fn with_duration(mut self, duration: i64) -> Packet {
        self.duration = Some(duration);
        self
    }

    /// `pts + duration`, when both are known.
    pub fn end_pts(&self) -> Option<i64> {
        self.pts?.checked_add(self.duration?)
    }

    /// True when decoding may start at this packet.
    pub fn is_keyframe(&self) -> bool {
        self.flags.keyframe
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buf_slices_share_one_allocation() {
        let buf = Buf::from_vec(vec![0, 1, 2, 3, 4, 5]);
        let mid = buf.slice(2..5);
        assert_eq!(&*mid, &[2, 3, 4]);
        assert_eq!(&*mid.slice(1..), &[3, 4]);
        assert_eq!(mid.len(), 3);
        assert!(Buf::new().is_empty());
        // Clone is a refcount bump, not a copy: same bytes, no reallocation.
        let clone = mid.clone();
        assert_eq!(clone, mid);
        assert_eq!(&*buf.slice(..), &[0, 1, 2, 3, 4, 5]);
    }

    #[test]
    #[should_panic(expected = "out of bounds")]
    fn buf_slice_rejects_out_of_range() {
        Buf::from_vec(vec![0, 1]).slice(0..3);
    }

    #[test]
    fn packet_construction() {
        let tb = TimeBase::from_rate(48_000);
        let mut p = Packet::new(0, tb, vec![0xff; 4]);
        assert!(!p.is_keyframe());
        assert_eq!(p.end_pts(), None);
        p = p.with_pts(1024).with_duration(1024);
        p.flags.keyframe = true;
        p.side_data
            .push(SideData::CodecConfig(Buf::copy_from_slice(&[0x12, 0x10])));
        assert_eq!(p.end_pts(), Some(2048));
        assert!(p.is_keyframe());
        assert_eq!(p.data.len(), 4);
        assert_eq!(p.time_base, TimeBase::new(1, 48_000));
    }
}
