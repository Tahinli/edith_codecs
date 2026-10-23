//! MPEG-1/2 video as a program stream carries it: PES packets cut the picture data
//! anywhere, and a PES timestamp belongs to the first picture that *starts* in that
//! packet, not to the packet's first byte. The pictures are re-cut on their start
//! codes in a buffer and every picture takes the timestamp of the packet its first
//! byte (the first header of its run) lies in.

use std::collections::VecDeque;

use crate::codec::{self, StreamKind};
use crate::track::{Au, MAX_PES, PesIn, Queued, TICKS, Track};

/// The PES a stretch of the buffer came from.
struct Mark {
    start: u64,
    end: u64,
    off: u64,
    pts: Option<i64>,
    dts: Option<i64>,
    bad: bool,
}

/// Unemitted video bytes with their start codes and PES provenance. Positions are
/// absolute (bytes pushed since the last [`VBuf::clear`]).
#[derive(Default)]
pub(crate) struct VBuf {
    data: Vec<u8>,
    /// Absolute position of `data[0]`.
    base: u64,
    /// Absolute position from which the next scan looks for start codes.
    scan_from: u64,
    /// `(position, code)` of every start code at or after `base`.
    codes: Vec<(u64, u8)>,
    marks: VecDeque<Mark>,
    /// Bytes are contiguous in the file (raw elementary stream): picture offsets are exact.
    pub exact: bool,
    /// The buffer begins at the start of an access unit.
    pub at_start: bool,
    /// Decode-order time (dts, else pts) of the last picture, for pictures that bring none.
    last_t: Option<i64>,
    /// Presentation time of temporal reference 0 of the current GOP, once known.
    gop0: Option<i64>,
    /// End of the display order so far (latest presentation time plus one frame).
    disp_end: Option<i64>,
}

impl VBuf {
    pub(crate) fn clear(&mut self) {
        self.data.clear();
        self.base = 0;
        self.scan_from = 0;
        self.codes.clear();
        self.marks.clear();
        self.at_start = false;
        self.last_t = None;
        self.gop0 = None;
        self.disp_end = None;
    }
}

impl Track {
    /// Feeds one video PES (or one chunk of a raw stream) to the picture cutter.
    pub(crate) fn ps_video_push(&mut self, ti: usize, p: &PesIn<'_>, q: &mut VecDeque<Queued>) {
        let b = &mut self.psv.buf;
        if b.data.len() + p.payload.len() > MAX_PES {
            // An unterminated picture: start over rather than grow without bound.
            let keep = b.base + b.data.len() as u64;
            b.clear();
            b.base = keep;
            b.scan_from = keep;
        }
        let start = b.base + b.data.len() as u64;
        b.data.extend_from_slice(p.payload);
        let end = start + p.payload.len() as u64;
        b.marks.push_back(Mark {
            start,
            end,
            off: p.off,
            pts: p.pts,
            dts: p.dts,
            bad: p.bad,
        });
        // New start codes: a code can straddle the join, hence the 3 bytes of overlap.
        let from = (b.scan_from.max(b.base) - b.base) as usize;
        let d = &b.data;
        let mut i = from;
        while i + 4 <= d.len() {
            if d[i + 2] > 1 {
                i += 3;
            } else if d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1 {
                b.codes.push((b.base + i as u64, d[i + 3]));
                i += 4;
            } else {
                i += 1;
            }
        }
        b.scan_from = b.base + i.min(d.len()) as u64;
        self.ps_cut(ti, q, false);
    }

    /// Emits every picture whose end is known; with `all`, the last one too.
    fn ps_cut(&mut self, ti: usize, q: &mut VecDeque<Queued>, all: bool) {
        let mut runs: Vec<u64> = self.ps_runs();
        runs.dedup();
        let last = if all {
            runs.len()
        } else {
            runs.len().saturating_sub(1)
        };
        if runs.is_empty() || (last == 0 && !all) {
            // Nothing complete. Bytes before the first run can never form a picture.
            if runs.is_empty() && !all {
                self.ps_trim_junk();
            }
            return;
        }
        let mut cuts: Vec<u64> = runs[..last.min(runs.len())].to_vec();
        let tail_end = self.psv.buf.base + self.psv.buf.data.len() as u64;
        let upto = if all {
            cuts.push(tail_end);
            tail_end
        } else {
            cuts.push(runs[last]);
            runs[last]
        };
        for w in cuts.windows(2) {
            self.ps_emit(ti, w[0], w[1], q);
        }
        let b = &mut self.psv.buf;
        let n = (upto - b.base) as usize;
        b.data.drain(..n);
        b.base = upto;
        b.at_start = !all;
        b.codes.retain(|&(p, _)| p >= upto);
        while b.marks.front().is_some_and(|m| m.end <= upto) {
            b.marks.pop_front();
        }
    }

    /// Where pictures (access units) start in the buffer.
    fn ps_runs(&self) -> Vec<u64> {
        let b = &self.psv.buf;
        let mut runs = Vec::new();
        let c = &b.codes;
        match self.kind {
            StreamKind::H264 | StreamKind::Hevc => {
                let hevc = self.kind == StreamKind::Hevc;
                let mut prev_vcl = false;
                let mut have = b.at_start;
                if have {
                    runs.push(b.base);
                }
                for &(pos, code) in c {
                    let (ty, vcl) = if hevc {
                        let t = (code >> 1) & 0x3F;
                        (t, t < 32)
                    } else {
                        let t = code & 0x1F;
                        (t, (1..=5).contains(&t))
                    };
                    let header = if hevc {
                        matches!(ty, 32..=35 | 39 | 41..=44 | 48..=55)
                    } else {
                        matches!(ty, 6..=9 | 14..=18)
                    };
                    let aud = if hevc { ty == 35 } else { ty == 9 };
                    let mut start = false;
                    if aud || (header && (prev_vcl || !have)) {
                        start = true;
                    } else if vcl && (prev_vcl || !have) {
                        // The first slice of a picture: first_mb_in_slice == 0 (H.264) or
                        // first_slice_segment_in_pic_flag (HEVC), the top bit after the header.
                        let at = (pos - b.base) as usize + 4 + usize::from(hevc);
                        match b.data.get(at) {
                            Some(&x) => start = x & 0x80 != 0,
                            None => break,
                        }
                    }
                    if start {
                        // The zero byte of a 4-byte start code belongs to the unit.
                        let s = if pos > b.base && b.data[(pos - b.base) as usize - 1] == 0 {
                            pos - 1
                        } else {
                            pos
                        };
                        runs.push(s);
                        have = true;
                        prev_vcl = false;
                    }
                    if vcl {
                        prev_vcl = true;
                    } else if !aud && !header {
                        // Parameter sets and other units keep the state.
                    }
                }
            }
            _ => {
                for (k, &(_, code)) in c.iter().enumerate() {
                    if code == 0 {
                        let mut j = k;
                        while j > 0 && matches!(c[j - 1].1, 0xB2 | 0xB3 | 0xB5 | 0xB8) {
                            j -= 1;
                        }
                        runs.push(c[j].0);
                    }
                }
            }
        }
        runs
    }

    /// Drops buffered bytes that precede every picture run, keeping a few bytes in case
    /// a start code straddles the next join.
    fn ps_trim_junk(&mut self) {
        let b = &mut self.psv.buf;
        let keep_from = (b.base + b.data.len() as u64).saturating_sub(3);
        if keep_from > b.base + (64 << 10) {
            let n = (keep_from - b.base) as usize;
            b.data.drain(..n);
            b.base = keep_from;
            b.codes.retain(|&(p, _)| p >= keep_from);
            while b.marks.front().is_some_and(|m| m.end <= keep_from) {
                b.marks.pop_front();
            }
        }
    }

    fn ps_emit(&mut self, ti: usize, from: u64, to: u64, q: &mut VecDeque<Queued>) {
        let b = &self.psv.buf;
        let piece = b.data[(from - b.base) as usize..(to - b.base) as usize].to_vec();
        let mark = b.marks.iter().find(|m| m.start <= from && from < m.end);
        let bad_any = b
            .marks
            .iter()
            .any(|m| m.bad && m.start < to && m.end > from);
        let (pts, dts, off) = match mark {
            Some(m) => {
                let off = if b.exact {
                    m.off + (from - m.start)
                } else {
                    m.off
                };
                (m.pts, m.dts, off)
            }
            None => (None, None, 0),
        };
        let bad = bad_any || mark.is_none();
        if matches!(self.kind, StreamKind::H264 | StreamKind::Hevc) {
            if let Some(m) = self
                .psv
                .buf
                .marks
                .iter_mut()
                .find(|m| m.start <= from && from < m.end)
            {
                m.pts = None;
                m.dts = None;
            }
            let au = Au {
                data: piece,
                pts,
                dts,
                off,
                rai: false,
                bad,
            };
            self.emit_annexb(ti, au, q);
            return;
        }
        let sc = codec::mpeg2_scan(&piece);
        if self.cfg.mpeg2.is_none() {
            self.cfg.mpeg2 = sc.facts.clone();
        }
        // A picture whose packet carried no timestamp (it shares the packet with the
        // one before): the decode time follows the previous picture by one frame and the
        // presentation time follows from the temporal reference, as ffmpeg's parser does
        // for the decode time (it leaves the presentation time unknown).
        let ft = self
            .cfg
            .mpeg2
            .as_ref()
            .map(|f| (TICKS * i64::from(f.rate.1)) / i64::from(f.rate.0.max(1)));
        let tr = sc.pictures.first().and_then(|&p| {
            Some((i64::from(*piece.get(p + 4)?) << 2) | i64::from(piece.get(p + 5)? >> 6))
        });
        let (mut pts, mut dts) = (pts, dts);
        let head = &piece[..sc.pictures.first().copied().unwrap_or(0)];
        let new_gop = head
            .windows(4)
            .any(|w| w == [0, 0, 1, 0xB8] || w == [0, 0, 1, 0xB3]);
        let b = &mut self.psv.buf;
        if new_gop {
            b.gop0 = None;
        }
        let mut derived = false;
        if pts.is_none() && dts.is_none() {
            if let (Some(ft), Some(last)) = (ft, b.last_t) {
                dts = Some(last + ft);
                derived = true;
            }
            if new_gop && b.gop0.is_none() {
                // The first picture of a GOP with no stamp: the GOP's display order
                // continues where the previous one ended (temporal reference 0 there).
                b.gop0 = b.disp_end;
            }
            if let (Some(ft), Some(tr), Some(g)) = (ft, tr, b.gop0) {
                pts = Some(g + tr * ft);
                derived = true;
            }
        }
        if let (Some(ft), Some(p)) = (ft, pts) {
            b.disp_end = Some(b.disp_end.map_or(p + ft, |e| e.max(p + ft)));
        }
        if let Some(t) = dts.or(pts) {
            b.last_t = Some(t);
        }
        if let (Some(ft), Some(tr), Some(p)) = (ft, tr, pts) {
            b.gop0 = Some(p - tr * ft);
        }
        // The same packet's timestamp must not be reused by a second picture.
        let used = self
            .psv
            .buf
            .marks
            .iter_mut()
            .find(|m| m.start <= from && from < m.end);
        if let Some(m) = used {
            m.pts = None;
            m.dts = None;
        }
        self.emit(
            q,
            Queued {
                track: ti,
                off,
                pts,
                dts,
                dur: None,
                key: sc.key,
                corrupt: bad,
                derived,
                data: piece,
            },
        );
    }

    /// Emits what is buffered (end of stream, a seek, a discontinuity).
    pub(crate) fn ps_flush(&mut self, ti: usize, q: &mut VecDeque<Queued>) {
        if !self.psv.on {
            return;
        }
        self.ps_cut(ti, q, true);
        self.psv.buf.clear();
    }
}
