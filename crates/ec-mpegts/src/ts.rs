//! Transport packet layer: packet-size detection, resynchronisation, the 4-byte
//! header and the adaptation field.

/// The sync byte every transport packet starts with.
pub(crate) const SYNC: u8 = 0x47;

/// Bytes of a bare transport packet.
pub(crate) const TS_LEN: usize = 188;

/// How many consecutive sync bytes at the packet stride make a candidate real.
const CONFIRM: usize = 4;

/// Packet stride and where the sync byte sits inside one stride.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Layout {
    /// 188 (bare), 192 (BDAV with a 4-byte `TP_extra_header`) or 204 (Reed-Solomon trailer).
    pub size: usize,
    /// 4 for BDAV, 0 otherwise.
    pub sync_off: usize,
}

impl Layout {
    pub(crate) const ALL: [Layout; 3] = [
        Layout {
            size: 188,
            sync_off: 0,
        },
        Layout {
            size: 192,
            sync_off: 4,
        },
        Layout {
            size: 204,
            sync_off: 0,
        },
    ];
}

/// True when `buf[at]` is a sync byte and the following strides agree: up to
/// `CONFIRM` more sync bytes. A candidate near the end of the buffer needs at
/// least one confirmation unless the buffer holds nothing more.
fn confirmed(buf: &[u8], at: usize, size: usize) -> bool {
    if buf.get(at) != Some(&SYNC) {
        return false;
    }
    let mut seen = 0;
    for k in 1..=CONFIRM {
        match buf.get(at + k * size) {
            Some(&SYNC) => seen += 1,
            Some(_) => return false,
            None => break,
        }
    }
    // Small inputs cannot confirm four strides; one is enough when it is all there is.
    seen >= CONFIRM.min(2) || (seen >= 1 && at + (seen + 1) * size + size > buf.len())
}

/// First packet start in `buf` at or after `from` whose sync byte is confirmed at
/// `layout`'s stride.
pub(crate) fn find_sync(buf: &[u8], from: usize, layout: Layout) -> Option<usize> {
    let mut i = from.checked_add(layout.sync_off)?;
    while i < buf.len() {
        let rest = buf.get(i..)?;
        let k = rest.iter().position(|&b| b == SYNC)?;
        let at = i + k;
        if confirmed(buf, at, layout.size) {
            return Some(at - layout.sync_off);
        }
        i = at + 1;
    }
    None
}

/// Detects the packet layout of `buf` (the head of a file): the earliest confirmed
/// sync position over the three strides. Returns the layout and the offset of the
/// first packet.
pub(crate) fn detect(buf: &[u8]) -> Option<(Layout, usize)> {
    let mut best: Option<(Layout, usize)> = None;
    for layout in Layout::ALL {
        if let Some(at) = find_sync(buf, 0, layout)
            && best.is_none_or(|(_, b)| at < b)
        {
            best = Some((layout, at));
        }
    }
    best
}

/// True when `buf` begins like a transport stream at some stride.
pub(crate) fn looks_like_ts(buf: &[u8]) -> bool {
    detect(buf).is_some_and(|(_, at)| at < 4 * TS_LEN)
}

/// A parsed 188-byte packet.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Header {
    pub pid: u16,
    pub tei: bool,
    pub pusi: bool,
    pub cc: u8,
    pub has_payload: bool,
    /// Payload range inside the 188 bytes (empty when there is none).
    pub payload: (usize, usize),
    pub discontinuity: bool,
    pub random_access: bool,
    /// PCR in 27 MHz units.
    pub pcr: Option<u64>,
}

/// Parses the header of `p`, which must hold at least 4 bytes. `None` for a packet
/// whose sync byte or adaptation field is broken.
pub(crate) fn parse(p: &[u8]) -> Option<Header> {
    let p = p.get(..TS_LEN)?;
    if p[0] != SYNC {
        return None;
    }
    let tei = p[1] & 0x80 != 0;
    let pusi = p[1] & 0x40 != 0;
    let pid = (u16::from(p[1] & 0x1f) << 8) | u16::from(p[2]);
    let afc = (p[3] >> 4) & 3;
    let cc = p[3] & 15;
    let mut h = Header {
        pid,
        tei,
        pusi,
        cc,
        has_payload: afc & 1 != 0,
        payload: (4, TS_LEN),
        discontinuity: false,
        random_access: false,
        pcr: None,
    };
    if afc == 0 {
        // Reserved: no payload, nothing to read.
        h.has_payload = false;
        h.payload = (TS_LEN, TS_LEN);
        return Some(h);
    }
    if afc & 2 != 0 {
        let len = usize::from(p[4]);
        let end = 5 + len;
        if end > TS_LEN {
            return None;
        }
        if len > 0 {
            let f = p[5];
            h.discontinuity = f & 0x80 != 0;
            h.random_access = f & 0x40 != 0;
            if f & 0x10 != 0 && len >= 7 {
                let b = &p[6..12];
                let base = (u64::from(b[0]) << 25)
                    | (u64::from(b[1]) << 17)
                    | (u64::from(b[2]) << 9)
                    | (u64::from(b[3]) << 1)
                    | u64::from(b[4] >> 7);
                let ext = (u64::from(b[4] & 1) << 8) | u64::from(b[5]);
                h.pcr = Some(base * 300 + ext);
            }
        }
        h.payload = if afc & 1 != 0 {
            (end, TS_LEN)
        } else {
            (TS_LEN, TS_LEN)
        };
    }
    Some(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(size: usize, sync_off: usize, n: usize) -> Vec<u8> {
        let mut v = vec![0u8; size * n];
        for k in 0..n {
            v[k * size + sync_off] = SYNC;
        }
        v
    }

    #[test]
    fn detects_the_three_strides() {
        for (size, off) in [(188, 0), (192, 4), (204, 0)] {
            let (l, at) = detect(&stream(size, off, 10)).unwrap();
            assert_eq!((l.size, at), (size, 0));
        }
    }

    #[test]
    fn resyncs_past_garbage() {
        let mut v = vec![0x11u8; 77];
        v.extend(stream(188, 0, 10));
        let (l, at) = detect(&v).unwrap();
        assert_eq!((l.size, at), (188, 77));
    }

    #[test]
    fn header_fields() {
        let mut p = [0xffu8; 188];
        p[0] = SYNC;
        p[1] = 0x41;
        p[2] = 0x00;
        p[3] = 0x30 | 5;
        p[4] = 7;
        p[5] = 0x50;
        p[6..12].copy_from_slice(&[0, 0, 0, 1, 0x80, 0x00]);
        let h = parse(&p).unwrap();
        assert!(h.pusi && h.random_access && !h.discontinuity);
        assert_eq!(h.pid, 0x100);
        assert_eq!(h.cc, 5);
        assert_eq!(h.pcr, Some(3 * 300));
        assert_eq!(h.payload, (12, 188));
    }
}
