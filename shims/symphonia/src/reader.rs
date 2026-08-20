//! The reader and decoder seats: `ec_probe` behind the incumbent's traits.

use symphonia_core::Result;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder, GenericAudioBufferRef};
use symphonia_core::codecs::from_ec_parameters;
use symphonia_core::formats::{FormatReader, SeekMode, SeekTo, SeekedTo, Track, TrackType};
use symphonia_core::packet::Packet;
use symphonia_core::units::{Duration, TimeBase, Timestamp};

use ec_core::registry::MediaType;

/// One open file: the tracks it declares, and its packets.
///
/// Track ids are the *container's* own — a Matroska `TrackNumber` — so a
/// caller that named one track by number gets that track back and not the one
/// that happened to be at the same index.
pub struct ProbeReader {
    inner: ec_probe::Reader,
    tracks: Vec<Track>,
    /// Audio track ids still owed their terminal, empty-data packet once the
    /// container itself has run out: `None` before that, then the queue,
    /// draining to empty rather than back to `None` so a second real EOF read
    /// hands out nothing more than once each.
    eos_pending: Option<std::vec::IntoIter<u32>>,
}

impl ProbeReader {
    /// Wrap an open [`ec_probe::Reader`].
    pub fn new(inner: ec_probe::Reader) -> ProbeReader {
        let tracks = inner
            .streams()
            .iter()
            .map(|s| Track {
                id: inner.native_id(s.index) as u32,
                track_type: match s.params.codec.media_type() {
                    MediaType::Audio => TrackType::Audio,
                    MediaType::Video => TrackType::Video,
                    MediaType::Subtitle => TrackType::Subtitle,
                },
                time_base: Some(TimeBase::from(s.time_base)),
                num_frames: s.duration.map(|d| d.max(0) as u64),
                start_ts: s.start_time.unwrap_or(0).max(0) as u64,
                delay: Some(u64::from(s.initial_padding)).filter(|d| *d > 0),
                codec_params: Some(from_ec_parameters(&s.params)),
                language: s.language.clone(),
            })
            .collect();
        ProbeReader {
            inner,
            tracks,
            eos_pending: None,
        }
    }

    /// The stream index behind a track id.
    fn stream_of(&self, id: u32) -> Option<u32> {
        self.inner.stream_of_native(u64::from(id))
    }

    /// The time base of a track id.
    fn base_of(&self, id: u32) -> ec_core::TimeBase {
        self.stream_of(id)
            .and_then(|s| self.inner.streams().iter().find(|i| i.index == s))
            .map(|s| s.time_base)
            .unwrap_or(ec_core::TimeBase::MILLIS)
    }
}

impl FormatReader for ProbeReader {
    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn default_track(&self, kind: TrackType) -> Option<&Track> {
        // The container's flagged track, not merely the first one of its kind:
        // a dual-audio remux that marked its second track default opens in that
        // language, which is what a caller asking for "the audio" means by it.
        let media = match kind {
            TrackType::Audio => MediaType::Audio,
            TrackType::Video => MediaType::Video,
            TrackType::Subtitle => MediaType::Subtitle,
        };
        let index = self.inner.default_stream(media)?.index;
        self.tracks.get(index as usize)
    }

    fn seek(&mut self, mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        let (time, track_id) = match to {
            SeekTo::Time { time, track_id } => (time.as_secs_f64(), track_id),
            SeekTo::TimeStamp { ts, track_id } => {
                let base = self.base_of(track_id);
                (ts.value() as f64 * base.as_secs_f64(), Some(track_id))
            }
        };
        let id = track_id
            .or_else(|| self.default_track(TrackType::Audio).map(|t| t.id))
            .unwrap_or(0);
        let stream = self.stream_of(id).unwrap_or(0);
        let base = self.base_of(id);
        let ticks = (time / base.as_secs_f64()) as i64;
        let landed = self.inner.seek(
            stream,
            ec_core::Timestamp::new(ticks, base),
            match mode {
                // Both modes land at or before the target: it is the only
                // landing a decoder can reach the exact sample from.
                SeekMode::Accurate | SeekMode::Coarse => ec_core::SeekMode::SyncBefore,
            },
        )?;
        Ok(SeekedTo {
            track_id: id,
            required_ts: Timestamp::new(ticks),
            actual_ts: Timestamp::new(landed.ticks),
        })
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        if let Some(pending) = &mut self.eos_pending {
            // Empty data is the flush signal `EcAudioDecoder::decode` reads:
            // a real packet is never zero bytes for any codec this family
            // decodes.
            return Ok(pending
                .next()
                .map(|id| Packet::new(id, Timestamp::new(0), Duration::new(0), &[])));
        }
        match self.inner.next_packet() {
            Ok(packet) => {
                let id = self.inner.native_id(packet.stream) as u32;
                let granule = ec_ogg::granule_of(&packet);
                let mut out = Packet::new(
                    id,
                    Timestamp::new(packet.pts.unwrap_or(0)),
                    Duration::new(packet.duration.unwrap_or(0).max(0) as u64),
                    &packet.data,
                );
                out.granule = granule;
                Ok(Some(out))
            }
            Err(e) if e.is_eof() => {
                // The container itself is exhausted: give every audio track
                // one last, empty packet so its decoder is told to flush the
                // hop it has been holding back, then report real EOF.
                let ids: Vec<u32> = self
                    .tracks
                    .iter()
                    .filter(|t| t.track_type == TrackType::Audio)
                    .map(|t| t.id)
                    .collect();
                let mut ids = ids.into_iter();
                let first = ids
                    .next()
                    .map(|id| Packet::new(id, Timestamp::new(0), Duration::new(0), &[]));
                self.eos_pending = Some(ids);
                Ok(first)
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// One decoder seat over [`ec_probe::AudioDecoder`].
pub(crate) struct EcAudioDecoder {
    inner: ec_probe::AudioDecoder,
    params: AudioCodecParameters,
    out: Vec<f32>,
}

impl EcAudioDecoder {
    pub(crate) fn new(
        inner: ec_probe::AudioDecoder,
        params: AudioCodecParameters,
    ) -> EcAudioDecoder {
        EcAudioDecoder {
            inner,
            params,
            out: Vec::new(),
        }
    }
}

impl AudioDecoder for EcAudioDecoder {
    fn decode(&mut self, packet: &Packet) -> Result<GenericAudioBufferRef<'_>> {
        if packet.data.is_empty() {
            // `ProbeReader::next_packet`'s terminal, empty packet: the file
            // has run out, and this is the signal to release whatever the
            // decoder was holding back rather than decode nothing.
            self.inner.flush(&mut self.out)?;
        } else {
            let rate = self.inner.sample_rate().max(1);
            let mut ec = ec_core::Packet::new(
                packet.track_id,
                ec_core::TimeBase::from_rate(rate),
                packet.data.as_ref(),
            );
            ec.pts = Some(packet.pts.value());
            ec.duration = Some(packet.dur.value() as i64);
            if let Some(granule) = packet.granule {
                ec.side_data.push(ec_ogg::granule_side_data(granule));
            }
            self.inner.decode(&ec, &mut self.out)?;
        }
        Ok(GenericAudioBufferRef::new(
            &self.out,
            self.inner.channels().max(1),
        ))
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn reset(&mut self) {
        self.inner.reset();
    }
}
