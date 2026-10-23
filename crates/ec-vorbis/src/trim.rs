//! Granule trimming: where an Ogg Vorbis stream starts and ends, in samples.
//!
//! A Vorbis block always decodes a whole window of samples, so the last packet
//! of a stream routinely reaches past the end the container states. Ogg says
//! where that end is: the granule position of the last page, which
//! `ec-ogg` attaches to the packet that finishes on it. Vorbis I (Annex A.2)
//! makes that granule the stream's exact length.
//!
//! The decoder counts its own samples from zero. A granule is an absolute
//! position, so the two are tied by an *offset* (`granule - decoded`), taken
//! from the first granule that arrives before any sample has been produced.
//! A stream that begins at position zero has offset zero and no front trim;
//! one that begins later, or whose first granule is ahead of its decoded
//! count, gets its head dropped. Later granules only move the end.
//!
//! Samples past the latest granule are not released: a later page may still
//! extend the stream, and the flush at end of stream cuts the rest.

use ec_core::{Packet, SideData};

/// The [`SideData::Custom`] kind `ec-ogg` attaches a page granule under. It is
/// `ec_ogg::GRANULE_KIND`; this crate does not depend on `ec-ogg`, so the
/// constant is restated here.
const GRANULE_KIND: u32 = u32::from_be_bytes(*b"OggS");

/// The granule position `ec-ogg` attached to `packet`, if it did.
pub(crate) fn granule_of(packet: &Packet) -> Option<i64> {
    packet.side_data.iter().find_map(|side| match side {
        SideData::Custom { kind, data } if *kind == GRANULE_KIND && data.len() == 8 => {
            Some(i64::from_le_bytes(data[..8].try_into().ok()?))
        }
        _ => None,
    })
}

/// Holds decoded samples until the granules say which of them are the stream's.
#[derive(Debug)]
pub(crate) struct GranuleTrim {
    /// Decoded samples not yet released, per channel in the output's order.
    pending: Vec<Vec<f32>>,
    /// Decoded-sample index of `pending[..][0]`.
    start: i64,
    /// Samples decoded so far, released or not.
    decoded: i64,
    /// `granule - decoded` at the first granule that arrived before any output,
    /// or zero when the first granule arrived later. Decoded index `i` is the
    /// stream's sample `i + offset` in granule terms.
    offset: Option<i64>,
    /// Exclusive decoded index the stream ends at, from the latest granule.
    end: Option<i64>,
}

impl GranuleTrim {
    /// An empty trim for `channels` output channels.
    pub(crate) fn new(channels: usize) -> GranuleTrim {
        GranuleTrim {
            pending: vec![Vec::new(); channels],
            start: 0,
            decoded: 0,
            offset: None,
            end: None,
        }
    }

    /// Samples decoded so far.
    pub(crate) fn decoded(&self) -> i64 {
        self.decoded
    }

    /// Take in one packet's decoded samples, one vector per output channel.
    pub(crate) fn push(&mut self, samples: Vec<Vec<f32>>) {
        let count = samples.first().map_or(0, Vec::len) as i64;
        for (pending, samples) in self.pending.iter_mut().zip(samples) {
            pending.extend(samples);
        }
        self.decoded += count;
    }

    /// Record a granule. `before` is `decoded()` as it stood before the
    /// packet carrying this granule was pushed.
    pub(crate) fn granule(&mut self, granule: i64, before: i64) {
        if self.offset.is_none() {
            let offset = if before == 0 {
                granule - self.decoded
            } else {
                0
            };
            self.offset = Some(offset);
            // Nothing was released before this packet, so the head to drop is
            // at the front of what is pending now.
            let front = (-offset).max(0);
            let len = self.pending.first().map_or(0, Vec::len) as i64;
            let drop = front.min(len).max(0) as usize;
            for pending in &mut self.pending {
                pending.drain(..drop);
            }
            self.start += drop as i64;
        }
        self.end = Some(granule - self.offset.unwrap_or(0));
    }

    /// Samples that are now final, one vector per channel. With `finish` set
    /// (end of stream), everything the last granule does not reach is dropped.
    pub(crate) fn release(&mut self, finish: bool) -> Vec<Vec<f32>> {
        let limit = match self.end {
            Some(end) => end.min(self.decoded),
            None => self.decoded,
        };
        let len = self.pending.first().map_or(0, Vec::len) as i64;
        let take = (limit - self.start).clamp(0, len) as usize;
        let out = self
            .pending
            .iter_mut()
            .map(|pending| pending.drain(..take).collect())
            .collect();
        self.start += take as i64;
        if finish {
            for pending in &mut self.pending {
                pending.clear();
            }
            self.start = self.decoded;
        }
        out
    }

    /// Forget everything, as a seek does.
    pub(crate) fn reset(&mut self) {
        for pending in &mut self.pending {
            pending.clear();
        }
        self.start = 0;
        self.decoded = 0;
        self.offset = None;
        self.end = None;
    }
}
