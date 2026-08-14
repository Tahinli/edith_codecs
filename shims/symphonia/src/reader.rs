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
                codec_params: Some(from_ec_parameters(&s.params)),
                language: s.language.clone(),
            })
            .collect();
        ProbeReader { inner, tracks }
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
        self.tracks.iter().find(|t| t.track_type == kind)
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
        match self.inner.next_packet() {
            Ok(packet) => {
                let id = self.inner.native_id(packet.stream) as u32;
                Ok(Some(Packet::new(
                    id,
                    Timestamp::new(packet.pts.unwrap_or(0)),
                    Duration::new(packet.duration.unwrap_or(0).max(0) as u64),
                    &packet.data,
                )))
            }
            Err(e) if e.is_eof() => Ok(None),
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
        let rate = self.inner.sample_rate().max(1);
        let mut ec = ec_core::Packet::new(
            packet.track_id,
            ec_core::TimeBase::from_rate(rate),
            packet.data.as_ref(),
        );
        ec.pts = Some(packet.pts.value());
        ec.duration = Some(packet.dur.value() as i64);
        self.inner.decode(&ec, &mut self.out)?;
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
