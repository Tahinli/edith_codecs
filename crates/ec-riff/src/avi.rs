//! RIFF/AVI audio chunk reader.
//!
//! This is the small half an audio probe needs: stream headers plus `movi`
//! sound chunks. Picture chunks are skipped.

use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};

use ec_core::{Error, Result};

const FALLBACK_SCAN: usize = 4 << 20;

/// One audio stream declared by an AVI file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AviAudioStream {
    /// AVI stream number, the two decimal digits at the start of chunk ids.
    pub index: u32,
    /// WAVE format tag from this stream's `strf` body.
    pub format_tag: u16,
    /// Channel count stated by the WAVEFORMAT header.
    pub channels: u16,
    /// Samples per second stated by the WAVEFORMAT header.
    pub sample_rate: u32,
    /// Block alignment stated by the WAVEFORMAT header.
    pub block_align: u16,
    /// Bits per sample stated by the WAVEFORMAT header.
    pub bits_per_sample: u16,
    /// Stream clock scale from `strh`.
    pub scale: u32,
    /// Stream clock rate from `strh`.
    pub rate: u32,
    /// Fixed sample byte size from `strh`, or zero for chunk-counted streams.
    pub sample_size: u32,
    /// Stream length in `strh` time units, when stated.
    pub length: u32,
}

/// One audio chunk from `movi`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AviPacket {
    /// AVI stream number.
    pub stream: u32,
    /// Chunk payload.
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
struct RawIndexEntry {
    stream: u32,
    id: [u8; 4],
    offset: u64,
    size: u32,
}

#[derive(Debug, Clone, Copy)]
struct AviSeekPoint {
    stream: u32,
    offset: u64,
    time: u64,
}

#[derive(Debug, Clone, Copy)]
struct OdmlRef {
    offset: u64,
    size: u32,
}

#[derive(Debug, Default)]
struct Hdrl {
    streams: Vec<AviAudioStream>,
    odml: Vec<OdmlRef>,
}

/// A reader over AVI audio chunks.
pub struct AviReader<R> {
    inner: R,
    streams: Vec<AviAudioStream>,
    movi_start: u64,
    movi_end: u64,
    pos: u64,
    index: Vec<AviSeekPoint>,
    pending: VecDeque<AviPacket>,
}

impl<R: Read + Seek> AviReader<R> {
    /// Parse the RIFF/AVI headers, leaving the reader at the start of `movi`.
    pub fn new(mut inner: R) -> Result<AviReader<R>> {
        let end = inner.seek(SeekFrom::End(0))?;
        inner.rewind()?;
        let mut head = [0u8; 12];
        read_full(&mut inner, &mut head)?;
        if &head[0..4] != b"RIFF" || &head[8..12] != b"AVI " {
            return Err(Error::corrupt("not a RIFF/AVI file"));
        }

        let mut hdrl = Hdrl::default();
        let mut movi = None;
        let mut idx1 = Vec::new();
        let mut pos = 12u64;
        while pos + 8 <= end {
            inner.seek(SeekFrom::Start(pos))?;
            let mut chunk = [0u8; 8];
            let got = read_some(&mut inner, &mut chunk)?;
            if got == 0 {
                break;
            }
            if got < 8 {
                return Err(Error::corrupt("AVI chunk header ends mid-field"));
            }
            let id = [chunk[0], chunk[1], chunk[2], chunk[3]];
            let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
            let data = pos + 8;
            let next = data.saturating_add(size).saturating_add(size & 1);
            if &id == b"LIST" {
                let mut kind = [0u8; 4];
                if size < 4 || read_some(&mut inner, &mut kind)? < 4 {
                    return Err(Error::corrupt("AVI LIST header ends mid-field"));
                }
                let body_start = data + 4;
                let body_end = data.saturating_add(size).min(end);
                match &kind {
                    b"hdrl" => hdrl = parse_hdrl(&mut inner, body_start, body_end)?,
                    b"movi" => movi = Some((body_start, body_end)),
                    _ => {}
                }
            } else if &id == b"idx1" {
                let mut body = vec![0; size as usize];
                read_full(&mut inner, &mut body)?;
                idx1 = parse_idx1_body(&body);
            }
            pos = next;
        }
        let (pos, movi_end) = movi.ok_or_else(|| Error::corrupt("AVI: no movi list"))?;
        let mut raw = Vec::new();
        if !idx1.is_empty() {
            raw.extend(resolve_idx1(
                &mut inner,
                pos,
                movi_end,
                &hdrl.streams,
                &idx1,
            )?);
        }
        for r in &hdrl.odml {
            raw.extend(parse_odml_standard(
                &mut inner,
                *r,
                &hdrl.streams,
                movi_end,
            )?);
        }
        let index = build_seek_points(&hdrl.streams, raw);
        Ok(AviReader {
            inner,
            streams: hdrl.streams,
            movi_start: pos,
            movi_end,
            pos,
            index,
            pending: VecDeque::new(),
        })
    }

    /// Audio streams declared in the header.
    pub fn audio_streams(&self) -> &[AviAudioStream] {
        &self.streams
    }

    /// True when the file supplied an AVI index this reader could use.
    pub fn has_index(&self) -> bool {
        !self.index.is_empty()
    }

    /// Return to the first chunk in `movi`.
    pub fn rewind(&mut self) {
        self.pos = self.movi_start;
        self.pending.clear();
    }

    /// Seek an audio stream by its `strh` time units and return the landing unit.
    pub fn seek_to_stream_time(&mut self, stream: u32, target: u64, after: bool) -> Result<u64> {
        self.pending.clear();
        if let Some(point) = self.index_point(stream, target, after) {
            self.pos = point.offset;
            return Ok(point.time);
        }
        let stream_info = self
            .streams
            .iter()
            .find(|s| s.index == stream)
            .ok_or_else(|| Error::corrupt(format!("AVI: no audio stream {stream}")))?;
        let estimate = if stream_info.length > 0 {
            let movi = self.movi_end.saturating_sub(self.movi_start);
            self.movi_start
                + movi
                    .saturating_mul(target)
                    .saturating_div(u64::from(stream_info.length))
        } else {
            self.movi_start
        };
        let start = estimate.clamp(self.movi_start, self.movi_end.saturating_sub(8));
        if let Some((offset, landed)) = self.scan_to_stream(stream, start, target)? {
            self.pos = offset;
            return Ok(landed);
        }
        self.pos = self.movi_end;
        Ok(target)
    }

    fn index_point(&self, stream: u32, target: u64, after: bool) -> Option<AviSeekPoint> {
        let points: Vec<_> = self
            .index
            .iter()
            .filter(|p| p.stream == stream)
            .copied()
            .collect();
        if points.is_empty() {
            return None;
        }
        let found = points.partition_point(|p| p.time <= target);
        let i = if after {
            found.min(points.len().saturating_sub(1))
        } else {
            found.saturating_sub(1)
        };
        points.get(i).copied()
    }

    /// The next audio chunk, or [`Error::Eof`] at the end.
    pub fn next_packet(&mut self) -> Result<AviPacket> {
        if let Some(packet) = self.pending.pop_front() {
            return Ok(packet);
        }
        while self.pos + 8 <= self.movi_end {
            self.inner.seek(SeekFrom::Start(self.pos))?;
            let mut chunk = [0u8; 8];
            let got = read_some(&mut self.inner, &mut chunk)?;
            if got == 0 || got < 8 {
                self.pos = self.movi_end;
                return Err(Error::Eof);
            }
            let id = [chunk[0], chunk[1], chunk[2], chunk[3]];
            let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
            let data = self.pos + 8;
            let padded = size.saturating_add(size & 1);
            let next = data.saturating_add(padded);
            if next > self.movi_end {
                self.pos = self.movi_end;
                return Err(Error::Eof);
            }
            self.pos = next;
            if &id == b"LIST" {
                if size >= 4 {
                    let mut kind = [0u8; 4];
                    if read_some(&mut self.inner, &mut kind)? < 4 {
                        self.pos = self.movi_end;
                        return Err(Error::Eof);
                    }
                    if &kind == b"rec " {
                        self.queue_packets_in(data + 4, data + size)?;
                        if let Some(packet) = self.pending.pop_front() {
                            return Ok(packet);
                        }
                    }
                }
                continue;
            }
            let Some(stream) = stream_number(&id).filter(|_| &id[2..4] == b"wb") else {
                continue;
            };
            if !self.streams.iter().any(|s| s.index == stream) {
                continue;
            }
            let data_buf = self.read_packet_body(data, size)?;
            return Ok(AviPacket {
                stream,
                data: data_buf,
            });
        }
        Err(Error::Eof)
    }

    fn queue_packets_in(&mut self, start: u64, end: u64) -> Result<()> {
        let mut pos = start;
        while pos + 8 <= end {
            self.inner.seek(SeekFrom::Start(pos))?;
            let mut chunk = [0u8; 8];
            if read_some(&mut self.inner, &mut chunk)? < 8 {
                break;
            }
            let id = [chunk[0], chunk[1], chunk[2], chunk[3]];
            let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
            let data = pos + 8;
            let next = data.saturating_add(size).saturating_add(size & 1);
            if next > end {
                break;
            }
            if &id == b"LIST" {
                if size >= 4 {
                    let mut kind = [0u8; 4];
                    if read_some(&mut self.inner, &mut kind)? < 4 {
                        break;
                    }
                    if &kind == b"rec " {
                        self.queue_packets_in(data + 4, data + size)?;
                    }
                }
            } else if let Some(stream) = stream_number(&id).filter(|_| &id[2..4] == b"wb")
                && self.streams.iter().any(|s| s.index == stream)
            {
                let data_buf = self.read_packet_body(data, size)?;
                self.pending.push_back(AviPacket {
                    stream,
                    data: data_buf,
                });
            }
            pos = next;
        }
        Ok(())
    }

    fn read_packet_body(&mut self, data: u64, size: u64) -> Result<Vec<u8>> {
        let mut data_buf = vec![0u8; size as usize];
        self.inner.seek(SeekFrom::Start(data))?;
        if read_some(&mut self.inner, &mut data_buf)? < data_buf.len() {
            self.pos = self.movi_end;
            return Err(Error::Eof);
        }
        Ok(data_buf)
    }

    fn scan_to_stream(
        &mut self,
        stream: u32,
        start: u64,
        target: u64,
    ) -> Result<Option<(u64, u64)>> {
        let limit = self
            .movi_end
            .min(start.saturating_add(FALLBACK_SCAN as u64));
        let mut pos = start;
        let mut buf = vec![0; 64 << 10];
        while pos + 8 <= limit {
            self.inner.seek(SeekFrom::Start(pos))?;
            let want = buf.len().min((limit - pos) as usize);
            let got = read_some(&mut self.inner, &mut buf[..want])?;
            if got < 8 {
                break;
            }
            for at in 0..got - 7 {
                let id = [buf[at], buf[at + 1], buf[at + 2], buf[at + 3]];
                if stream_number(&id) != Some(stream) || &id[2..4] != b"wb" {
                    continue;
                }
                let size =
                    u32::from_le_bytes([buf[at + 4], buf[at + 5], buf[at + 6], buf[at + 7]]) as u64;
                let offset = pos + at as u64;
                if offset + 8 + size + (size & 1) <= self.movi_end {
                    return Ok(Some((offset, target)));
                }
            }
            pos += got.saturating_sub(7) as u64;
        }
        Ok(None)
    }
}

fn build_seek_points(streams: &[AviAudioStream], mut raw: Vec<RawIndexEntry>) -> Vec<AviSeekPoint> {
    raw.sort_by_key(|e| e.offset);
    let mut clocks = vec![0u64; streams.len()];
    let mut out = Vec::new();
    for e in raw {
        let Some(i) = streams.iter().position(|s| s.index == e.stream) else {
            continue;
        };
        let time = clocks[i];
        clocks[i] = clocks[i].saturating_add(chunk_units(streams[i], u64::from(e.size)));
        out.push(AviSeekPoint {
            stream: e.stream,
            offset: e.offset,
            time,
        });
    }
    out
}

fn chunk_units(stream: AviAudioStream, size: u64) -> u64 {
    match stream.sample_size {
        0 => 1,
        n => (size / u64::from(n)).max(u64::from(size > 0)),
    }
}

fn parse_hdrl<R: Read + Seek>(inner: &mut R, start: u64, end: u64) -> Result<Hdrl> {
    let mut hdrl = Hdrl::default();
    let mut stream_no = 0u32;
    let mut pos = start;
    while pos + 8 <= end {
        inner.seek(SeekFrom::Start(pos))?;
        let mut chunk = [0u8; 8];
        if read_some(inner, &mut chunk)? < 8 {
            break;
        }
        let id = [chunk[0], chunk[1], chunk[2], chunk[3]];
        let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
        let data = pos + 8;
        let next = data.saturating_add(size).saturating_add(size & 1);
        if &id == b"LIST" {
            let mut kind = [0u8; 4];
            if size >= 4 && read_some(inner, &mut kind)? == 4 && &kind == b"strl" {
                let parsed = parse_strl(inner, stream_no, data + 4, data + size)?;
                if let Some(stream) = parsed.stream {
                    hdrl.streams.push(stream);
                    hdrl.odml.extend(parsed.odml);
                }
                stream_no += 1;
            }
        }
        pos = next;
    }
    Ok(hdrl)
}

#[derive(Default)]
struct Strl {
    stream: Option<AviAudioStream>,
    odml: Vec<OdmlRef>,
}

#[derive(Clone, Copy)]
struct StreamTiming {
    scale: u32,
    rate: u32,
    sample_size: u32,
    length: u32,
}

impl Default for StreamTiming {
    fn default() -> Self {
        StreamTiming {
            scale: 1,
            rate: 1,
            sample_size: 0,
            length: 0,
        }
    }
}

fn parse_strl<R: Read + Seek>(inner: &mut R, stream_no: u32, start: u64, end: u64) -> Result<Strl> {
    let mut audio = false;
    let mut fmt = None;
    let mut timing = StreamTiming::default();
    let mut odml = Vec::new();
    let mut pos = start;
    while pos + 8 <= end {
        inner.seek(SeekFrom::Start(pos))?;
        let mut chunk = [0u8; 8];
        if read_some(inner, &mut chunk)? < 8 {
            break;
        }
        let id = [chunk[0], chunk[1], chunk[2], chunk[3]];
        let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
        let data = pos + 8;
        let next = data.saturating_add(size).saturating_add(size & 1);
        match &id {
            b"strh" => {
                let mut body = vec![0u8; size.min(64) as usize];
                read_full(inner, &mut body)?;
                audio = body.get(0..4) == Some(&b"auds"[..]);
                if body.len() >= 48 {
                    timing = StreamTiming {
                        scale: le_u32(&body, 20).filter(|n| *n > 0).unwrap_or(1),
                        rate: le_u32(&body, 24).filter(|n| *n > 0).unwrap_or(1),
                        length: le_u32(&body, 32).unwrap_or(0),
                        sample_size: le_u32(&body, 44).unwrap_or(0),
                    };
                }
            }
            b"strf" => {
                let mut body = vec![0u8; size.min(18) as usize];
                read_full(inner, &mut body)?;
                if body.len() >= 16 {
                    fmt = Some(AviAudioStream {
                        index: stream_no,
                        format_tag: u16::from_le_bytes([body[0], body[1]]),
                        channels: u16::from_le_bytes([body[2], body[3]]),
                        sample_rate: u32::from_le_bytes([body[4], body[5], body[6], body[7]]),
                        block_align: u16::from_le_bytes([body[12], body[13]]),
                        bits_per_sample: u16::from_le_bytes([body[14], body[15]]),
                        scale: timing.scale,
                        rate: timing.rate,
                        sample_size: timing.sample_size,
                        length: timing.length,
                    });
                }
            }
            b"indx" => {
                let mut body = vec![0; size as usize];
                read_full(inner, &mut body)?;
                odml.extend(parse_indx_refs(&body));
            }
            _ => {}
        }
        pos = next;
    }
    Ok(Strl {
        stream: audio.then_some(fmt).flatten(),
        odml,
    })
}

fn parse_idx1_body(body: &[u8]) -> Vec<RawIndexEntry> {
    let mut out = Vec::new();
    for entry in body.chunks_exact(16) {
        let id = [entry[0], entry[1], entry[2], entry[3]];
        let Some(stream) = stream_number(&id).filter(|_| &id[2..4] == b"wb") else {
            continue;
        };
        out.push(RawIndexEntry {
            stream,
            id,
            offset: u64::from(u32::from_le_bytes([
                entry[8], entry[9], entry[10], entry[11],
            ])),
            size: u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]),
        });
    }
    out
}

fn resolve_idx1<R: Read + Seek>(
    inner: &mut R,
    movi_start: u64,
    movi_end: u64,
    streams: &[AviAudioStream],
    entries: &[RawIndexEntry],
) -> Result<Vec<RawIndexEntry>> {
    let Some(first) = entries
        .iter()
        .find(|e| streams.iter().any(|s| s.index == e.stream))
    else {
        return Ok(Vec::new());
    };
    let base = [0, movi_start, movi_start.saturating_sub(4)]
        .into_iter()
        .find(|base| {
            chunk_matches(inner, base.saturating_add(first.offset), first.id, movi_end)
                .unwrap_or(false)
        })
        .unwrap_or(movi_start);
    let mut out = Vec::new();
    for e in entries {
        if !streams.iter().any(|s| s.index == e.stream) {
            continue;
        }
        let offset = base.saturating_add(e.offset);
        if chunk_matches(inner, offset, e.id, movi_end)? {
            out.push(RawIndexEntry { offset, ..*e });
        }
    }
    Ok(out)
}

fn parse_indx_refs(body: &[u8]) -> Vec<OdmlRef> {
    if body.len() < 24 || body[3] != 0 {
        return Vec::new();
    }
    let entries = le_u32(body, 4).unwrap_or(0) as usize;
    let mut out = Vec::new();
    let mut at = 24usize;
    for _ in 0..entries {
        if at + 16 > body.len() {
            break;
        }
        let offset = le_u64(body, at).unwrap_or(0);
        let size = le_u32(body, at + 8).unwrap_or(0);
        if offset > 0 && size >= 24 {
            out.push(OdmlRef { offset, size });
        }
        at += 16;
    }
    out
}

fn parse_odml_standard<R: Read + Seek>(
    inner: &mut R,
    index: OdmlRef,
    streams: &[AviAudioStream],
    movi_end: u64,
) -> Result<Vec<RawIndexEntry>> {
    if index.offset + 8 > movi_end.saturating_add(1 << 30) {
        return Ok(Vec::new());
    }
    inner.seek(SeekFrom::Start(index.offset))?;
    let mut head = [0; 8];
    if read_some(inner, &mut head)? < 8 {
        return Ok(Vec::new());
    }
    let size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
    let mut body = vec![0; size.min(index.size as usize)];
    read_full(inner, &mut body)?;
    let raw = parse_standard_body(&body, streams);
    Ok(raw
        .into_iter()
        .filter_map(|e| resolve_standard_entry(inner, e, movi_end).transpose())
        .collect::<Result<Vec<_>>>()?)
}

fn parse_standard_body(body: &[u8], streams: &[AviAudioStream]) -> Vec<RawIndexEntry> {
    if body.len() < 24 || body[3] != 1 {
        return Vec::new();
    }
    let longs = u16::from_le_bytes([body[0], body[1]]).max(2) as usize;
    let entries = le_u32(body, 4).unwrap_or(0) as usize;
    let id = [body[8], body[9], body[10], body[11]];
    let Some(stream) = stream_number(&id).filter(|_| &id[2..4] == b"wb") else {
        return Vec::new();
    };
    if !streams.is_empty() && !streams.iter().any(|s| s.index == stream) {
        return Vec::new();
    }
    let base = le_u64(body, 12).unwrap_or(0);
    let step = longs * 4;
    let mut at = 24usize;
    let mut out = Vec::new();
    for _ in 0..entries {
        if at + 8 > body.len() {
            break;
        }
        out.push(RawIndexEntry {
            stream,
            id,
            offset: base.saturating_add(u64::from(le_u32(body, at).unwrap_or(0))),
            size: le_u32(body, at + 4).unwrap_or(0) & 0x7fff_ffff,
        });
        at += step;
    }
    out
}

fn resolve_standard_entry<R: Read + Seek>(
    inner: &mut R,
    entry: RawIndexEntry,
    movi_end: u64,
) -> Result<Option<RawIndexEntry>> {
    for offset in [entry.offset, entry.offset.saturating_sub(8)] {
        if chunk_matches(inner, offset, entry.id, movi_end)? {
            return Ok(Some(RawIndexEntry { offset, ..entry }));
        }
    }
    Ok(None)
}

fn chunk_matches<R: Read + Seek>(
    inner: &mut R,
    offset: u64,
    id: [u8; 4],
    end: u64,
) -> Result<bool> {
    if offset + 8 > end {
        return Ok(false);
    }
    inner.seek(SeekFrom::Start(offset))?;
    let mut head = [0; 8];
    if read_some(inner, &mut head)? < 8 {
        return Ok(false);
    }
    Ok(head[0..4] == id
        && offset + 8 + u64::from(u32::from_le_bytes([head[4], head[5], head[6], head[7]])) <= end)
}

fn stream_number(id: &[u8; 4]) -> Option<u32> {
    let a = id[0].checked_sub(b'0')?;
    let b = id[1].checked_sub(b'0')?;
    (a < 10 && b < 10).then_some(u32::from(a) * 10 + u32::from(b))
}

fn le_u32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn le_u64(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(data.get(at..at + 8)?.try_into().ok()?))
}

fn read_some(r: &mut impl Read, buf: &mut [u8]) -> Result<usize> {
    let mut at = 0;
    while at < buf.len() {
        match r.read(&mut buf[at..]) {
            Ok(0) => break,
            Ok(n) => at += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Ok(at)
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> Result<()> {
    if read_some(r, buf)? == buf.len() {
        Ok(())
    } else {
        Err(Error::corrupt("AVI header ends mid-field"))
    }
}
