//! JPEG decoding (ITU-T T.81): baseline and extended sequential DCT (SOF0,
//! SOF1) and progressive DCT (SOF2), Huffman coded, with restart markers.
//!
//! Chroma is upsampled with the triangle ("fancy") filter rather than by pixel
//! replication, because replication puts a visible 2-pixel staircase on every
//! saturated edge of a 4:2:0 photo — the artefact people notice first.
//!
//! What is refused, and why it is refused rather than approximated:
//!
//! - Arithmetic-coded and lossless/hierarchical JPEGs (SOF3, SOF5-15): a
//!   different entropy coder and a different process, not a variant of this one.
//! - CMYK and YCCK (4-component, Adobe APP14): decoding them without the ICC
//!   profile they normally travel with produces colours that look right in a
//!   thumbnail and are wrong in print. Named refusal beats plausible garbage.
//!
//! EXIF orientation is parsed into [`Metadata::orientation`] and *not* applied
//! — matching the incumbent `image` crate, so a caller sees the same pixels
//! from either.

use crate::upsample::upsample;
use crate::{AllocBudget, Image, ImageFormat, Info, Limits, Metadata, Pixels};
use ec_core::{Error, Result};

/// Zig-zag order: coefficient `k` of a scan sits at natural index `ZIGZAG[k]`.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// A canonical Huffman table, as DHT specifies it.
#[derive(Clone, Default)]
struct HuffTable {
    /// `(length, value)` for every 8-bit prefix, `length == 0` when the code
    /// is longer than 8 bits and the slow path is needed.
    lut: Vec<(u8, u8)>,
    /// Largest code of each length, or -1 when that length is unused.
    max_code: [i32; 17],
    /// Index into `values` of the first code of each length.
    val_ptr: [usize; 17],
    /// Smallest code of each length.
    min_code: [i32; 17],
    values: Vec<u8>,
}

impl HuffTable {
    fn build(counts: &[u8; 16], values: Vec<u8>) -> Result<HuffTable> {
        let mut table = HuffTable {
            lut: vec![(0, 0); 256],
            max_code: [-1; 17],
            val_ptr: [0; 17],
            min_code: [0; 17],
            values,
        };
        let mut code = 0i32;
        let mut k = 0usize;
        for len in 1..=16usize {
            table.val_ptr[len] = k;
            table.min_code[len] = code;
            for _ in 0..counts[len - 1] {
                if k >= table.values.len() {
                    return Err(Error::corrupt("JPEG: DHT has fewer values than counts"));
                }
                if len <= 8 {
                    // Every 8-bit prefix starting with this code maps to it.
                    let shift = 8 - len;
                    let base = (code as usize) << shift;
                    if base + (1 << shift) > 256 {
                        return Err(Error::corrupt("JPEG: over-long Huffman code"));
                    }
                    for slot in base..base + (1 << shift) {
                        table.lut[slot] = (len as u8, table.values[k]);
                    }
                }
                code += 1;
                k += 1;
            }
            table.max_code[len] = code - 1;
            code <<= 1;
        }
        Ok(table)
    }
}

/// The entropy-coded segment reader: MSB-first bits, `FF 00` unstuffed.
struct BitReader<'a> {
    data: &'a [u8],
    at: usize,
    bits: u32,
    count: u32,
    /// Set once the reader has had to invent bits: the scan's data ran into a
    /// marker or ended. The fill runs several bytes ahead of the decoder, so
    /// this says "the tail is padding", never "stop here" — a scan that stops
    /// on it loses the last blocks it still had bits for.
    hit_marker: bool,
    /// Padding bytes (zeros invented past the data or a marker) loaded since the last restart.
    padding: u32,
}

/// Padding bytes loaded into one restart interval before the scan counts as out of data. A
/// well-formed interval loads at most four (the fill runs up to four bytes ahead), so eight
/// is never reached by a valid file.
const EXHAUSTED_PADDING: u32 = 8;

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8], at: usize) -> BitReader<'a> {
        BitReader {
            data,
            at,
            bits: 0,
            count: 0,
            hit_marker: false,
            padding: 0,
        }
    }

    /// Whether the scan has run out of data: it has consumed far enough into the invented bits
    /// that the rest of the picture cannot be real. The decoder then stops, as libjpeg does, and
    /// the blocks it did not reach keep their zero coefficients.
    fn exhausted(&self) -> bool {
        self.padding >= EXHAUSTED_PADDING
    }

    fn fill(&mut self) {
        while self.count <= 24 {
            let byte = match self.data.get(self.at) {
                Some(&0xff) => match self.data.get(self.at + 1) {
                    Some(&0x00) => {
                        self.at += 2;
                        0xff
                    }
                    _ => {
                        self.hit_marker = true;
                        self.padding += 1;
                        0
                    }
                },
                Some(&b) => {
                    self.at += 1;
                    b
                }
                None => {
                    self.hit_marker = true;
                    self.padding += 1;
                    0
                }
            };
            self.bits |= u32::from(byte) << (24 - self.count);
            self.count += 8;
        }
    }

    fn peek8(&mut self) -> u8 {
        self.fill();
        (self.bits >> 24) as u8
    }

    fn consume(&mut self, n: u32) {
        self.bits <<= n;
        self.count = self.count.saturating_sub(n);
    }

    fn bit(&mut self) -> u32 {
        self.fill();
        let b = self.bits >> 31;
        self.consume(1);
        b
    }

    fn bits(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        self.fill();
        let v = self.bits >> (32 - n);
        self.consume(n);
        v
    }

    fn decode(&mut self, table: &HuffTable) -> Result<u8> {
        let prefix = self.peek8();
        let (len, value) = table.lut[prefix as usize];
        if len != 0 {
            self.consume(u32::from(len));
            return Ok(value);
        }
        // Codes longer than the lookahead: walk lengths the canonical way.
        let mut code = i32::from(prefix);
        self.consume(8);
        for len in 9..=16usize {
            code = (code << 1) | self.bit() as i32;
            if table.max_code[len] >= code && code >= table.min_code[len] {
                let index = table.val_ptr[len] + (code - table.min_code[len]) as usize;
                return table
                    .values
                    .get(index)
                    .copied()
                    .ok_or_else(|| Error::corrupt("JPEG: Huffman code outside its table"));
            }
        }
        Err(Error::corrupt("JPEG: no Huffman code matches the bits"))
    }

    /// T.81 F.2.2.1 EXTEND: sign-extend an `s`-bit magnitude.
    fn receive_extend(&mut self, s: u8) -> i32 {
        if s == 0 {
            return 0;
        }
        let v = self.bits(u32::from(s)) as i32;
        if v < (1 << (s - 1)) {
            v - (1 << s) + 1
        } else {
            v
        }
    }

    /// Restart: byte-align and step over the RSTn marker.
    fn restart(&mut self) -> bool {
        self.bits = 0;
        self.count = 0;
        self.padding = 0;
        // Skip to the marker, which a well-formed stream is already at.
        while self.at + 1 < self.data.len() {
            if self.data[self.at] == 0xff && (0xd0..=0xd7).contains(&self.data[self.at + 1]) {
                self.at += 2;
                return true;
            }
            self.at += 1;
        }
        self.hit_marker = true;
        false
    }
}

/// One colour component of the frame.
#[derive(Clone)]
struct Component {
    id: u8,
    h: usize,
    v: usize,
    quant: usize,
    /// Blocks across and down, padded out to whole MCUs.
    blocks_w: usize,
    blocks_h: usize,
    /// Blocks the picture actually covers (`blocks_w` may be larger).
    used_w: usize,
    used_h: usize,
    coeffs: Vec<i16>,
    dc_pred: i32,
}

/// Everything the markers before the first scan established.
struct Frame {
    progressive: bool,
    width: u32,
    height: u32,
    hmax: usize,
    vmax: usize,
    mcus_w: usize,
    mcus_h: usize,
    components: Vec<Component>,
}

fn be16(data: &[u8], at: usize) -> Result<usize> {
    data.get(at..at + 2)
        .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
        .ok_or_else(|| Error::corrupt("JPEG: truncated segment"))
}

/// Dimensions and EXIF orientation from the headers alone.
///
/// Walks the marker segments up to the start of scan, so it reads no entropy-coded
/// data. The frame header gives the size; the first APP1 `Exif` segment gives the
/// orientation (1 when there is none).
pub fn info(data: &[u8]) -> Result<Info> {
    let mut at = 2;
    let mut frame = None;
    let mut orientation = 1;
    while let Some(marker) = next_marker(data, &mut at) {
        match marker {
            0xc0..=0xc2 => {
                let len = be16(data, at)?;
                let seg = data
                    .get(at + 2..at + len)
                    .ok_or_else(|| Error::corrupt("JPEG: truncated SOF"))?;
                if seg.len() < 6 {
                    return Err(Error::corrupt("JPEG: short SOF"));
                }
                frame.get_or_insert((
                    u32::from(u16::from_be_bytes([seg[3], seg[4]])),
                    u32::from(u16::from_be_bytes([seg[1], seg[2]])),
                ));
            }
            0xd8 | 0xd9 => {}
            0xda => break,
            _ => {
                let len = be16(data, at)?;
                if marker == 0xe1 && orientation == 1 {
                    orientation = data
                        .get(at + 2..at + len)
                        .and_then(exif_orientation)
                        .unwrap_or(1);
                }
                at += len.max(2);
            }
        }
        if frame.is_some() && orientation != 1 {
            break;
        }
    }
    let (width, height) = frame.ok_or_else(|| Error::corrupt("JPEG: no frame header"))?;
    Ok(Info {
        format: ImageFormat::Jpeg,
        width,
        height,
        orientation,
    })
}

/// Step to the next marker, returning its byte and leaving `at` on its payload.
fn next_marker(data: &[u8], at: &mut usize) -> Option<u8> {
    while *at < data.len() {
        if data[*at] != 0xff {
            *at += 1;
            continue;
        }
        let mut scan = *at + 1;
        while data.get(scan) == Some(&0xff) {
            scan += 1;
        }
        let marker = *data.get(scan)?;
        *at = scan + 1;
        if marker != 0x00 {
            return Some(marker);
        }
    }
    None
}

/// Everything the marker segments of one JPEG established, with every scan decoded into
/// coefficients but nothing inverse-transformed yet.
struct Parsed {
    frame: Frame,
    quant: [[u16; 64]; 4],
    meta: Metadata,
    adobe: Option<u8>,
    /// Offset just past the EOI marker (or the end of the data when there was none).
    end: usize,
}

/// Decode a JPEG.
pub fn decode(data: &[u8], limits: Limits) -> Result<Image> {
    let parsed = parse(data, limits, false)?;
    let pixels = reconstruct(&parsed.frame, &parsed.quant, parsed.adobe, limits)?;
    Ok(Image {
        width: parsed.frame.width,
        height: parsed.frame.height,
        pixels,
        meta: parsed.meta,
    })
}

/// Walk the marker segments and entropy-decode every scan. `mjpeg` preloads the Annex K
/// default Huffman tables as tables 0 and 1, for the AVI1/MJPG packets that carry no DHT.
fn parse(data: &[u8], limits: Limits, mjpeg: bool) -> Result<Parsed> {
    let mut quant = [[0u16; 64]; 4];
    let mut dc_tables: Vec<HuffTable> = vec![HuffTable::default(); 4];
    let mut ac_tables: Vec<HuffTable> = vec![HuffTable::default(); 4];
    if mjpeg {
        install_default_tables(&mut dc_tables, &mut ac_tables)?;
    }
    let mut restart_interval = 0usize;
    let mut frame: Option<Frame> = None;
    let mut scanned = false;
    let mut meta = Metadata::default();
    let mut adobe_transform: Option<u8> = None;
    let mut end = data.len();

    let mut at = 2;
    while let Some(marker) = next_marker(data, &mut at) {
        match marker {
            0xd8 => continue,
            0xd9 => {
                end = at;
                break;
            }
            0x01 | 0xd0..=0xd7 => continue,
            _ => {}
        }
        let len = be16(data, at)?;
        if len < 2 {
            return Err(Error::corrupt("JPEG: segment length below 2"));
        }
        let seg = data
            .get(at + 2..at + len)
            .ok_or_else(|| Error::corrupt("JPEG: truncated segment"))?;
        match marker {
            0xdb => read_dqt(seg, &mut quant)?,
            0xc4 => read_dht(seg, &mut dc_tables, &mut ac_tables)?,
            0xdd => {
                if seg.len() < 2 {
                    return Err(Error::corrupt("JPEG: short DRI"));
                }
                restart_interval = usize::from(u16::from_be_bytes([seg[0], seg[1]]));
            }
            0xc0..=0xc2 => {
                let f = read_sof(seg, marker == 0xc2, limits)?;
                if mjpeg {
                    // Refuse an unmappable layout before any scan is decoded.
                    subsampling_of(&f)?;
                }
                frame = Some(f);
            }
            0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => {
                return Err(Error::unsupported(
                    format!("JPEG SOF{}", marker & 0x0f),
                    "arithmetic-coded, lossless and hierarchical JPEG are a different process",
                ));
            }
            0xe1 => {
                // The first EXIF segment is the one that counts.
                if meta.orientation.is_none() {
                    meta.orientation = exif_orientation(seg);
                }
            }
            0xee => {
                if seg.starts_with(b"Adobe") {
                    adobe_transform = seg.last().copied();
                }
            }
            0xda => {
                let frame = frame
                    .as_mut()
                    .ok_or_else(|| Error::corrupt("JPEG: scan before frame header"))?;
                scanned = true;
                at = read_scan(
                    data,
                    at + len,
                    seg,
                    frame,
                    &dc_tables,
                    &ac_tables,
                    restart_interval,
                )?;
                continue;
            }
            _ => {}
        }
        at += len;
    }

    let frame = frame.ok_or_else(|| Error::corrupt("JPEG: no frame header"))?;
    // A frame with no scan carries no picture data. Refused rather than returned as a grey
    // canvas, so a header that declares a large picture costs nothing to refuse.
    if !scanned {
        return Err(Error::corrupt("JPEG: a frame header with no scan"));
    }
    Ok(Parsed {
        frame,
        quant,
        meta,
        adobe: adobe_transform,
        end,
    })
}

fn read_dqt(mut seg: &[u8], quant: &mut [[u16; 64]; 4]) -> Result<()> {
    while !seg.is_empty() {
        let pq = seg[0] >> 4;
        let tq = usize::from(seg[0] & 0x0f);
        if tq >= 4 {
            return Err(Error::corrupt(format!("JPEG: quant table id {tq}")));
        }
        let need = if pq == 1 { 128 } else { 64 };
        let body = seg
            .get(1..1 + need)
            .ok_or_else(|| Error::corrupt("JPEG: truncated DQT"))?;
        for k in 0..64 {
            quant[tq][ZIGZAG[k]] = if pq == 1 {
                u16::from_be_bytes([body[k * 2], body[k * 2 + 1]])
            } else {
                u16::from(body[k])
            };
        }
        seg = &seg[1 + need..];
    }
    Ok(())
}

fn read_dht(mut seg: &[u8], dc: &mut [HuffTable], ac: &mut [HuffTable]) -> Result<()> {
    while !seg.is_empty() {
        let class = seg[0] >> 4;
        let id = usize::from(seg[0] & 0x0f);
        if id >= 4 || class > 1 {
            return Err(Error::corrupt(format!("JPEG: DHT class {class} id {id}")));
        }
        let counts: [u8; 16] = seg
            .get(1..17)
            .ok_or_else(|| Error::corrupt("JPEG: truncated DHT"))?
            .try_into()
            .expect("16 bytes");
        let total: usize = counts.iter().map(|&c| usize::from(c)).sum();
        let values = seg
            .get(17..17 + total)
            .ok_or_else(|| Error::corrupt("JPEG: truncated DHT values"))?
            .to_vec();
        let table = HuffTable::build(&counts, values)?;
        if class == 0 {
            dc[id] = table
        } else {
            ac[id] = table
        }
        seg = &seg[17 + total..];
    }
    Ok(())
}

fn read_sof(seg: &[u8], progressive: bool, limits: Limits) -> Result<Frame> {
    if seg.len() < 6 {
        return Err(Error::corrupt("JPEG: short SOF"));
    }
    if seg[0] != 8 {
        return Err(Error::unsupported(
            format!("{}-bit JPEG", seg[0]),
            "only 8-bit sample precision is implemented",
        ));
    }
    let height = u32::from(u16::from_be_bytes([seg[1], seg[2]]));
    let width = u32::from(u16::from_be_bytes([seg[3], seg[4]]));
    limits.check(width, height)?;
    let count = usize::from(seg[5]);
    if count == 0 || count > 4 {
        return Err(Error::corrupt(format!("JPEG: {count} components")));
    }
    if count == 4 {
        return Err(Error::unsupported(
            "4-component (CMYK/YCCK) JPEG",
            "the ink colours need the ICC profile this decoder does not carry",
        ));
    }
    if count == 2 {
        return Err(Error::unsupported(
            "2-component JPEG",
            "no colour interpretation is defined for it",
        ));
    }
    let body = seg
        .get(6..6 + count * 3)
        .ok_or_else(|| Error::corrupt("JPEG: truncated SOF components"))?;
    let mut components = Vec::with_capacity(count);
    for c in body.as_chunks::<3>().0.iter() {
        let (h, v) = (usize::from(c[1] >> 4), usize::from(c[1] & 0x0f));
        if h == 0 || v == 0 || h > 4 || v > 4 {
            return Err(Error::corrupt(format!("JPEG: sampling factors {h}x{v}")));
        }
        components.push(Component {
            id: c[0],
            h,
            v,
            quant: usize::from(c[2] & 0x03),
            blocks_w: 0,
            blocks_h: 0,
            used_w: 0,
            used_h: 0,
            coeffs: Vec::new(),
            dc_pred: 0,
        });
    }
    let hmax = components.iter().map(|c| c.h).max().unwrap_or(1);
    let vmax = components.iter().map(|c| c.v).max().unwrap_or(1);
    let mcus_w = (width as usize).div_ceil(8 * hmax);
    let mcus_h = (height as usize).div_ceil(8 * vmax);
    // Coefficient storage is budgeted across every component, not one at a time: three
    // full-size planes each under the per-buffer cap can still add up to far past it.
    let mut budget = AllocBudget::default();
    for c in &mut components {
        c.blocks_w = mcus_w * c.h;
        c.blocks_h = mcus_h * c.v;
        c.used_w = ((width as usize) * c.h).div_ceil(8 * hmax);
        c.used_h = ((height as usize) * c.v).div_ceil(8 * vmax);
        let cells = c
            .blocks_w
            .checked_mul(c.blocks_h)
            .and_then(|b| b.checked_mul(64))
            .ok_or_else(|| Error::corrupt("JPEG: component geometry overflows"))?;
        limits.check_bytes("JPEG coefficients", cells.checked_mul(2))?;
        budget.spend(&limits, "JPEG coefficients", cells * 2)?;
        c.coeffs = vec![0i16; cells];
    }
    Ok(Frame {
        progressive,
        width,
        height,
        hmax,
        vmax,
        mcus_w,
        mcus_h,
        components,
    })
}

/// Decode one scan; returns the offset just past its entropy-coded data.
fn read_scan(
    data: &[u8],
    start: usize,
    header: &[u8],
    frame: &mut Frame,
    dc_tables: &[HuffTable],
    ac_tables: &[HuffTable],
    restart_interval: usize,
) -> Result<usize> {
    if header.is_empty() {
        return Err(Error::corrupt("JPEG: empty SOS"));
    }
    let ns = usize::from(header[0]);
    let spec = header
        .get(1..1 + ns * 2)
        .ok_or_else(|| Error::corrupt("JPEG: truncated SOS"))?;
    let tail = header
        .get(1 + ns * 2..4 + ns * 2)
        .ok_or_else(|| Error::corrupt("JPEG: truncated SOS tail"))?;
    let (ss, se) = (usize::from(tail[0]), usize::from(tail[1]));
    let (ah, al) = (tail[2] >> 4, tail[2] & 0x0f);
    if se > 63 || ss > se {
        return Err(Error::corrupt(format!("JPEG: spectral range {ss}..{se}")));
    }

    // Which frame components this scan carries, and with which tables.
    let mut in_scan = Vec::with_capacity(ns);
    for c in spec.as_chunks::<2>().0.iter() {
        let index = frame
            .components
            .iter()
            .position(|f| f.id == c[0])
            .ok_or_else(|| Error::corrupt(format!("JPEG: scan names component {}", c[0])))?;
        in_scan.push((index, usize::from(c[1] >> 4), usize::from(c[1] & 0x0f)));
    }
    // A scan naming no components has nothing to decode and no geometry to walk.
    if in_scan.is_empty() {
        return Err(Error::corrupt("JPEG: a scan with no components"));
    }
    for c in &mut frame.components {
        c.dc_pred = 0;
    }

    let mut bits = BitReader::new(data, start);
    let mut eob_run = 0u32;
    let interleaved = ns > 1;
    let (units_w, units_h) = if interleaved {
        (frame.mcus_w, frame.mcus_h)
    } else {
        let c = &frame.components[in_scan[0].0];
        (c.used_w, c.used_h)
    };
    let mut since_restart = 0usize;

    for unit in 0..units_w * units_h {
        if restart_interval > 0 && since_restart == restart_interval {
            since_restart = 0;
            eob_run = 0;
            for c in &mut frame.components {
                c.dc_pred = 0;
            }
            if !bits.restart() {
                break;
            }
        }
        if bits.exhausted() {
            break;
        }
        since_restart += 1;
        let (ux, uy) = (unit % units_w, unit / units_w);
        for &(index, dc_id, ac_id) in &in_scan {
            let (h, v) = {
                let c = &frame.components[index];
                if interleaved { (c.h, c.v) } else { (1, 1) }
            };
            for by in 0..v {
                for bx in 0..h {
                    let (block_x, block_y) = if interleaved {
                        (ux * h + bx, uy * v + by)
                    } else {
                        (ux, uy)
                    };
                    let component = &mut frame.components[index];
                    if block_x >= component.blocks_w || block_y >= component.blocks_h {
                        continue;
                    }
                    let offset = (block_y * component.blocks_w + block_x) * 64;
                    // A progressive DC scan names an AC table it never uses
                    // (and vice versa), so the table is only required where it
                    // is actually read from.
                    let dc = dc_tables.get(dc_id).filter(|t| !t.values.is_empty());
                    let ac = ac_tables.get(ac_id).filter(|t| !t.values.is_empty());
                    if frame.progressive {
                        decode_progressive_block(
                            &mut bits,
                            component,
                            offset,
                            dc,
                            ac,
                            ss,
                            se,
                            ah,
                            al,
                            &mut eob_run,
                        )?;
                    } else {
                        decode_baseline_block(&mut bits, component, offset, dc, ac)?;
                    }
                }
            }
        }
    }

    // Walk to the next marker that is not a restart: that is where the next
    // segment begins.
    let mut at = bits.at.max(start);
    while at + 1 < data.len() {
        if data[at] == 0xff {
            let m = data[at + 1];
            if m != 0x00 && !(0xd0..=0xd7).contains(&m) && m != 0xff {
                return Ok(at);
            }
        }
        at += 1;
    }
    Ok(data.len())
}

/// A scan may name a Huffman table that no DHT ever defined; that is a corrupt
/// file, not a table to index into.
fn table_or_corrupt<'a>(table: Option<&'a HuffTable>, which: &str) -> Result<&'a HuffTable> {
    match table {
        Some(table) if !table.values.is_empty() => Ok(table),
        _ => Err(Error::corrupt(format!(
            "JPEG: scan names a {which} Huffman table the file never sent"
        ))),
    }
}

fn decode_baseline_block(
    bits: &mut BitReader<'_>,
    component: &mut Component,
    offset: usize,
    dc: Option<&HuffTable>,
    ac: Option<&HuffTable>,
) -> Result<()> {
    let (dc, ac) = (table_or_corrupt(dc, "DC")?, table_or_corrupt(ac, "AC")?);
    let t = bits.decode(dc)?;
    if t > 15 {
        return Err(Error::corrupt(format!("JPEG: DC magnitude {t}")));
    }
    let diff = bits.receive_extend(t);
    component.dc_pred = component.dc_pred.wrapping_add(diff);
    component.coeffs[offset] = component.dc_pred.clamp(-32768, 32767) as i16;

    let mut k = 1usize;
    while k < 64 {
        let rs = bits.decode(ac)?;
        let (r, s) = ((rs >> 4) as usize, rs & 0x0f);
        if s == 0 {
            if r != 15 {
                break;
            }
            k += 16;
            continue;
        }
        k += r;
        if k > 63 {
            break;
        }
        let value = bits.receive_extend(s);
        component.coeffs[offset + ZIGZAG[k]] = value.clamp(-32768, 32767) as i16;
        k += 1;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decode_progressive_block(
    bits: &mut BitReader<'_>,
    component: &mut Component,
    offset: usize,
    dc: Option<&HuffTable>,
    ac: Option<&HuffTable>,
    ss: usize,
    se: usize,
    ah: u8,
    al: u8,
    eob_run: &mut u32,
) -> Result<()> {
    if ss == 0 {
        // DC scan: first pass codes the difference, later passes one bit each.
        if ah == 0 {
            let t = bits.decode(table_or_corrupt(dc, "DC")?)?;
            if t > 15 {
                return Err(Error::corrupt(format!("JPEG: DC magnitude {t}")));
            }
            let diff = bits.receive_extend(t);
            component.dc_pred = component.dc_pred.wrapping_add(diff);
            component.coeffs[offset] = (component.dc_pred << al).clamp(-32768, 32767) as i16;
        } else if bits.bit() == 1 {
            component.coeffs[offset] |= 1 << al;
        }
        return Ok(());
    }

    let ac = table_or_corrupt(ac, "AC")?;
    if ah == 0 {
        // AC first pass: values, with runs of end-of-block.
        if *eob_run > 0 {
            *eob_run -= 1;
            return Ok(());
        }
        let mut k = ss;
        while k <= se {
            let rs = bits.decode(ac)?;
            let (r, s) = (u32::from(rs >> 4), rs & 0x0f);
            if s == 0 {
                if r < 15 {
                    *eob_run = (1 << r) - 1;
                    if r > 0 {
                        *eob_run += bits.bits(r);
                    }
                    break;
                }
                k += 16;
                continue;
            }
            k += r as usize;
            if k > se {
                break;
            }
            let value = bits.receive_extend(s);
            component.coeffs[offset + ZIGZAG[k]] = ((value) << al).clamp(-32768, 32767) as i16;
            k += 1;
        }
        return Ok(());
    }

    // AC refinement: correction bits for coefficients already non-zero, plus
    // newly significant ones placed by the run length.
    let positive = 1i16 << al;
    let negative = -1i16 << al;
    let mut k = ss;
    if *eob_run > 0 {
        *eob_run -= 1;
        refine_nonzero(bits, component, offset, k, se, positive, negative);
        return Ok(());
    }
    while k <= se {
        let rs = bits.decode(ac)?;
        let (mut r, s) = (i32::from(rs >> 4), rs & 0x0f);
        let mut value = 0i16;
        if s == 0 {
            if r < 15 {
                *eob_run = (1 << r) - 1;
                if r > 0 {
                    *eob_run += bits.bits(r as u32);
                }
                refine_nonzero(bits, component, offset, k, se, positive, negative);
                break;
            }
        } else {
            value = if bits.bit() == 1 { positive } else { negative };
        }
        while k <= se {
            let cell = offset + ZIGZAG[k];
            if component.coeffs[cell] != 0 {
                if bits.bit() == 1 && component.coeffs[cell] & positive == 0 {
                    component.coeffs[cell] = if component.coeffs[cell] >= 0 {
                        component.coeffs[cell].saturating_add(positive)
                    } else {
                        component.coeffs[cell].saturating_add(negative)
                    };
                }
            } else {
                if r == 0 {
                    if value != 0 {
                        component.coeffs[cell] = value;
                    }
                    k += 1;
                    break;
                }
                r -= 1;
            }
            k += 1;
        }
    }
    Ok(())
}

/// Apply one correction bit to each already-non-zero coefficient in `k..=se`.
fn refine_nonzero(
    bits: &mut BitReader<'_>,
    component: &mut Component,
    offset: usize,
    k: usize,
    se: usize,
    positive: i16,
    negative: i16,
) {
    for k in k..=se {
        let cell = offset + ZIGZAG[k];
        if component.coeffs[cell] != 0 && bits.bit() == 1 && component.coeffs[cell] & positive == 0
        {
            component.coeffs[cell] = if component.coeffs[cell] >= 0 {
                component.coeffs[cell].saturating_add(positive)
            } else {
                component.coeffs[cell].saturating_add(negative)
            };
        }
    }
}

/// Fixed-point 1-D IDCT basis: `round(c_v * cos((2x+1) v pi / 16) * 2^13)`.
///
/// Thirteen fractional bits, and eight of them kept between the two passes,
/// so the separable inversion lands within a count of the exact transform —
/// the difference against another decoder is then its rounding, not ours.
const IDCT_BASIS: [[i32; 8]; 8] = build_basis();

const fn build_basis() -> [[i32; 8]; 8] {
    // Values computed once, off the same formula the tests re-derive in f64.
    [
        [2896, 2896, 2896, 2896, 2896, 2896, 2896, 2896],
        [4017, 3406, 2276, 799, -799, -2276, -3406, -4017],
        [3784, 1567, -1567, -3784, -3784, -1567, 1567, 3784],
        [3406, -799, -4017, -2276, 2276, 4017, 799, -3406],
        [2896, -2896, -2896, 2896, 2896, -2896, -2896, 2896],
        [2276, -4017, 799, 3406, -3406, -799, 4017, -2276],
        [1567, -3784, 3784, -1567, -1567, 3784, -3784, 1567],
        [799, -2276, 3406, -4017, 4017, -3406, 2276, -799],
    ]
}

/// Dequantize and invert one 8x8 block into 0..=255 samples.
fn idct_block(coeffs: &[i16], quant: &[u16; 64], out: &mut [u8; 64]) {
    // A block whose only coefficient is the DC term (the common case, and every block of a
    // scan that runs out of data) has one non-zero column of the first pass, so the second pass
    // gives each output column one value. This is the general transform below, computed once
    // per column rather than 512 times.
    if coeffs[1..].iter().all(|&c| c == 0) {
        let dc = i64::from(coeffs[0]) * i64::from(quant[0]);
        let value = (dc * i64::from(IDCT_BASIS[0][0]) + 16) >> 5;
        for x in 0..8 {
            let column = ((value * i64::from(IDCT_BASIS[0][x]) + (1 << 20)) >> 21) + 128;
            let column = column.clamp(0, 255) as u8;
            for y in 0..8 {
                out[y * 8 + x] = column;
            }
        }
        return;
    }
    let mut tmp = [0i64; 64];
    for x in 0..8 {
        // Columns first; the all-zero column shortcut is what makes a flat
        // photo background cheap.
        let nonzero = (1..8).any(|v| coeffs[v * 8 + x] != 0);
        let dc = i64::from(coeffs[x]) * i64::from(quant[x]);
        if !nonzero {
            let value = (dc * i64::from(IDCT_BASIS[0][0]) + 16) >> 5;
            for y in 0..8 {
                tmp[y * 8 + x] = value;
            }
            continue;
        }
        for y in 0..8 {
            let mut sum = 0i64;
            for v in 0..8 {
                let c = i64::from(coeffs[v * 8 + x]) * i64::from(quant[v * 8 + x]);
                sum += c * i64::from(IDCT_BASIS[v][y]);
            }
            tmp[y * 8 + x] = (sum + 16) >> 5;
        }
    }
    for y in 0..8 {
        for x in 0..8 {
            let mut sum = 0i64;
            for u in 0..8 {
                sum += tmp[y * 8 + u] * i64::from(IDCT_BASIS[u][x]);
            }
            let value = ((sum + (1 << 20)) >> 21) + 128;
            out[y * 8 + x] = value.clamp(0, 255) as u8;
        }
    }
}

/// Inverse-transform every block of a component into a plane of samples.
fn component_plane(component: &Component, quant: &[u16; 64]) -> Vec<u8> {
    let width = component.blocks_w * 8;
    let mut plane = vec![0u8; width * component.blocks_h * 8];
    let mut block = [0u8; 64];
    for by in 0..component.blocks_h {
        for bx in 0..component.blocks_w {
            let offset = (by * component.blocks_w + bx) * 64;
            idct_block(&component.coeffs[offset..offset + 64], quant, &mut block);
            for row in 0..8 {
                let dst = (by * 8 + row) * width + bx * 8;
                plane[dst..dst + 8].copy_from_slice(&block[row * 8..row * 8 + 8]);
            }
        }
    }
    plane
}

/// BT.601 full-range YCbCr to RGB, in libjpeg's 16-bit fixed point.
fn ycbcr_to_rgb(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    let y = i32::from(y) << 16;
    let cb = i32::from(cb) - 128;
    let cr = i32::from(cr) - 128;
    let r = y + 91881 * cr;
    let g = y - 22554 * cb - 46802 * cr;
    let b = y + 116130 * cb;
    [
        ((r + 32768) >> 16).clamp(0, 255) as u8,
        ((g + 32768) >> 16).clamp(0, 255) as u8,
        ((b + 32768) >> 16).clamp(0, 255) as u8,
    ]
}

fn reconstruct(
    frame: &Frame,
    quant: &[[u16; 64]; 4],
    adobe: Option<u8>,
    limits: Limits,
) -> Result<Pixels> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let out_per_pixel = if frame.components.len() == 1 { 1 } else { 3 };
    limits.check_output(frame.width, frame.height, out_per_pixel)?;
    let planes: Vec<Vec<u8>> = frame
        .components
        .iter()
        .map(|c| {
            let plane = component_plane(c, &quant[c.quant]);
            let pw = c.blocks_w * 8;
            let ph = c.blocks_h * 8;
            // Full-resolution extent this component covers before upsampling.
            let cw = (w * c.h).div_ceil(frame.hmax);
            let ch = (h * c.v).div_ceil(frame.vmax);
            let mut cropped: Vec<u8> = Vec::with_capacity(cw.min(pw) * ch.min(ph));
            for y in 0..ch.min(ph) {
                cropped.extend_from_slice(&plane[y * pw..y * pw + cw.min(pw)]);
            }
            upsample(&cropped, cw.min(pw), ch.min(ph), w, h)
        })
        .collect();

    match frame.components.len() {
        1 => Ok(Pixels::L8(planes.into_iter().next().ok_or_else(|| {
            Error::corrupt("JPEG: a single-component frame with no plane")
        })?)),
        3 => {
            // Component ids 'R','G','B' (or an Adobe transform of 0) mean the
            // samples are already RGB and must not be matrixed again.
            let ids: Vec<u8> = frame.components.iter().map(|c| c.id).collect();
            let already_rgb = adobe == Some(0) || ids == *b"RGB";
            // Every plane is out_w * out_h, so the three zip to the picture with no bounds checks.
            let mut out = vec![0u8; w * h * 3];
            for (px, ((&a, &b), &c)) in out
                .as_chunks_mut::<3>()
                .0
                .iter_mut()
                .zip(planes[0].iter().zip(&planes[1]).zip(&planes[2]))
            {
                *px = if already_rgb { [a, b, c] } else { ycbcr_to_rgb(a, b, c) };
            }
            Ok(Pixels::Rgb8(out))
        }
        n => Err(Error::unsupported(
            format!("{n}-component JPEG"),
            "no colour interpretation is defined for it",
        )),
    }
}

// ---------------------------------------------------------------------------------------------
// Video path: Motion JPEG frames straight to planar YUV.
// ---------------------------------------------------------------------------------------------

const DC_LUM_COUNTS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const DC_CHR_COUNTS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const AC_LUM_COUNTS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
const AC_CHR_COUNTS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
const AC_LUM_VALUES: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
    0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5,
    0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
    0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];
const AC_CHR_VALUES: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0,
    0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26,
    0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5,
    0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
    0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
    0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];

/// T.81 Annex K.3 tables as table 0 (luminance) and table 1 (chrominance), which is what an
/// AVI1/MJPG packet without a DHT segment relies on.
fn install_default_tables(dc: &mut [HuffTable], ac: &mut [HuffTable]) -> Result<()> {
    dc[0] = HuffTable::build(&DC_LUM_COUNTS, (0..12).collect())?;
    dc[1] = HuffTable::build(&DC_CHR_COUNTS, (0..12).collect())?;
    ac[0] = HuffTable::build(&AC_LUM_COUNTS, AC_LUM_VALUES.to_vec())?;
    ac[1] = HuffTable::build(&AC_CHR_COUNTS, AC_CHR_VALUES.to_vec())?;
    Ok(())
}

/// Chroma layout of a decoded video frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subsampling {
    /// One component.
    Gray,
    /// Chroma halved both ways.
    Yuv420,
    /// Chroma halved across.
    Yuv422,
    /// Chroma at full size.
    Yuv444,
}

/// One 8-bit plane, tightly packed (`stride == width`) and cropped to the picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YuvPlane {
    /// Samples per row.
    pub width: usize,
    /// Rows.
    pub height: usize,
    /// `width * height` samples.
    pub data: Vec<u8>,
}

/// A JPEG decoded to its native planes: no chroma upsampling, no colour conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YuvImage {
    /// Luma width.
    pub width: u32,
    /// Luma height.
    pub height: u32,
    /// Chroma layout.
    pub subsampling: Subsampling,
    /// Y (then Cb, Cr unless [`Subsampling::Gray`]).
    pub planes: Vec<YuvPlane>,
    /// Whether the file was progressive (SOF2).
    pub progressive: bool,
    /// Offset in the input just past this image's EOI marker (the end of the input when the
    /// image has none): where a second field of a two-field packet would start.
    pub end: usize,
}

/// FFmpeg's `simple_idct` constants (cos(i*pi/16) * sqrt(2) * 2^14, rounded).
const W1: i64 = 22725;
const W2: i64 = 21407;
const W3: i64 = 19266;
const W4: i64 = 16383;
const W5: i64 = 12873;
const W6: i64 = 8867;
const W7: i64 = 4520;
const ROW_SHIFT: u32 = 11;
const COL_SHIFT: u32 = 20;

/// One row pass of the integer IDCT the JPEG-world decoders (libavcodec's mjpeg, in
/// particular) use. Sums are formed in 64 bits and truncated to 32 where C's `int` would wrap.
fn simple_idct_row(row: &mut [i16; 8]) {
    if row[1..].iter().all(|&c| c == 0) {
        let v = ((i32::from(row[0]) << 3) & 0xffff) as u16 as i16;
        *row = [v; 8];
        return;
    }
    let r: [i64; 8] = std::array::from_fn(|i| i64::from(row[i]));
    let a0 = W4 * r[0] + (1 << (ROW_SHIFT - 1));
    let (mut a0, mut a1, mut a2, mut a3) = (a0, a0, a0, a0);
    a0 += W2 * r[2];
    a1 += W6 * r[2];
    a2 -= W6 * r[2];
    a3 -= W2 * r[2];
    let mut b0 = W1 * r[1] + W3 * r[3];
    let mut b1 = W3 * r[1] - W7 * r[3];
    let mut b2 = W5 * r[1] - W1 * r[3];
    let mut b3 = W7 * r[1] - W5 * r[3];
    a0 += W4 * r[4] + W6 * r[6];
    a1 += -W4 * r[4] - W2 * r[6];
    a2 += -W4 * r[4] + W2 * r[6];
    a3 += W4 * r[4] - W6 * r[6];
    b0 += W5 * r[5] + W7 * r[7];
    b1 += -W1 * r[5] - W5 * r[7];
    b2 += W7 * r[5] + W3 * r[7];
    b3 += W3 * r[5] - W1 * r[7];
    let t = |v: i64| ((v as i32) >> ROW_SHIFT) as i16;
    row[0] = t(a0 + b0);
    row[7] = t(a0 - b0);
    row[1] = t(a1 + b1);
    row[6] = t(a1 - b1);
    row[2] = t(a2 + b2);
    row[5] = t(a2 - b2);
    row[3] = t(a3 + b3);
    row[4] = t(a3 - b3);
}

/// One column pass, clamped straight to samples.
fn simple_idct_col(block: &[i16; 64], x: usize, out: &mut [u8; 64]) {
    let c: [i64; 8] = std::array::from_fn(|i| i64::from(block[i * 8 + x]));
    let a0 = W4 * (c[0] + ((1 << (COL_SHIFT - 1)) / W4));
    let (mut a0, mut a1, mut a2, mut a3) = (a0, a0, a0, a0);
    a0 += W2 * c[2];
    a1 += W6 * c[2];
    a2 -= W6 * c[2];
    a3 -= W2 * c[2];
    let mut b0 = W1 * c[1] + W3 * c[3];
    let mut b1 = W3 * c[1] - W7 * c[3];
    let mut b2 = W5 * c[1] - W1 * c[3];
    let mut b3 = W7 * c[1] - W5 * c[3];
    a0 += W4 * c[4];
    a1 -= W4 * c[4];
    a2 -= W4 * c[4];
    a3 += W4 * c[4];
    b0 += W5 * c[5];
    b1 -= W1 * c[5];
    b2 += W7 * c[5];
    b3 += W3 * c[5];
    a0 += W6 * c[6];
    a1 -= W2 * c[6];
    a2 += W2 * c[6];
    a3 -= W6 * c[6];
    b0 += W7 * c[7];
    b1 -= W5 * c[7];
    b2 += W3 * c[7];
    b3 -= W1 * c[7];
    let q = |v: i64| (((v as i32) >> COL_SHIFT).clamp(0, 255)) as u8;
    out[x] = q(a0 + b0);
    out[8 + x] = q(a1 + b1);
    out[16 + x] = q(a2 + b2);
    out[24 + x] = q(a3 + b3);
    out[32 + x] = q(a3 - b3);
    out[40 + x] = q(a2 - b2);
    out[48 + x] = q(a1 - b1);
    out[56 + x] = q(a0 - b0);
}

/// Dequantize and invert one block the way libavcodec's mjpeg decoder does: the level shift
/// is 1024 added to the dequantized DC term, products wrap to 16 bits, and the transform is
/// `simple_idct`.
fn idct_block_video(coeffs: &[i16], quant: &[u16; 64], out: &mut [u8; 64]) {
    let mut block = [0i16; 64];
    for i in 0..64 {
        block[i] = (i32::from(coeffs[i]) * i32::from(quant[i])) as i16;
    }
    block[0] = (i32::from(coeffs[0]) * i32::from(quant[0]) + 1024) as i16;
    for r in 0..8 {
        let row: &mut [i16; 8] = (&mut block[r * 8..r * 8 + 8]).try_into().expect("8");
        simple_idct_row(row);
    }
    for x in 0..8 {
        simple_idct_col(&block, x, out);
    }
}

fn subsampling_of(frame: &Frame) -> Result<Subsampling> {
    match frame.components.len() {
        1 => Ok(Subsampling::Gray),
        3 => {
            let c = &frame.components;
            let (y, cb, cr) = (&c[0], &c[1], &c[2]);
            if (cb.h, cb.v) != (cr.h, cr.v) || (y.h, y.v) != (frame.hmax, frame.vmax) {
                return Err(Error::unsupported(
                    format!(
                        "JPEG sampling {}x{} {}x{} {}x{}",
                        y.h, y.v, cb.h, cb.v, cr.h, cr.v
                    ),
                    "only gray, 4:2:0, 4:2:2 and 4:4:4 map to a pixel format",
                ));
            }
            match (y.h / cb.h.max(1), y.v / cb.v.max(1), y.h % cb.h, y.v % cb.v) {
                (1, 1, 0, 0) => Ok(Subsampling::Yuv444),
                (2, 1, 0, 0) => Ok(Subsampling::Yuv422),
                (2, 2, 0, 0) => Ok(Subsampling::Yuv420),
                _ => Err(Error::unsupported(
                    format!(
                        "JPEG sampling {}x{} {}x{} {}x{}",
                        y.h, y.v, cb.h, cb.v, cr.h, cr.v
                    ),
                    "only gray, 4:2:0, 4:2:2 and 4:4:4 map to a pixel format",
                )),
            }
        }
        n => Err(Error::unsupported(
            format!("{n}-component JPEG"),
            "no colour interpretation is defined for it",
        )),
    }
}

/// Decode a Motion JPEG frame to its native planes.
///
/// Chroma stays at the file's own subsampling and is not converted: the planes are what
/// libavcodec's `mjpeg` decoder returns (`yuvj420p`, `yuvj422p`, `yuvj444p`, `gray`),
/// sample for sample. The default Huffman tables (T.81 Annex K) stand in for a missing DHT,
/// as AVI1/MJPG packets need. Progressive (SOF2) frames decode; 12-bit precision, arithmetic
/// coding, CMYK, RGB-coded and non-4:2:0/4:2:2/4:4:4 layouts are refused by name.
pub fn decode_yuv(data: &[u8], limits: Limits) -> Result<YuvImage> {
    if data.len() < 4 || data[0] != 0xff || data[1] != 0xd8 {
        return Err(Error::corrupt("JPEG: no SOI marker"));
    }
    let parsed = parse(data, limits, true)?;
    let frame = &parsed.frame;
    let subsampling = subsampling_of(frame)?;
    if subsampling != Subsampling::Gray {
        let ids: Vec<u8> = frame.components.iter().map(|c| c.id).collect();
        if parsed.adobe == Some(0) || ids == *b"RGB" {
            return Err(Error::unsupported(
                "RGB-coded Motion JPEG",
                "the samples are not YCbCr and no planar RGB format is wired",
            ));
        }
    }
    let (w, h) = (frame.width as usize, frame.height as usize);
    let mut planes = Vec::with_capacity(frame.components.len());
    for c in &frame.components {
        let q = &parsed.quant[c.quant];
        let pw = c.blocks_w * 8;
        let cw = (w * c.h).div_ceil(frame.hmax);
        let ch = (h * c.v).div_ceil(frame.vmax);
        let mut data = vec![0u8; cw * ch];
        let mut block = [0u8; 64];
        for by in 0..c.blocks_h {
            if by * 8 >= ch {
                break;
            }
            for bx in 0..c.blocks_w {
                if bx * 8 >= cw {
                    break;
                }
                let off = (by * c.blocks_w + bx) * 64;
                idct_block_video(&c.coeffs[off..off + 64], q, &mut block);
                let n = (cw - bx * 8).min(8);
                for row in 0..8.min(ch - by * 8) {
                    let dst = (by * 8 + row) * cw + bx * 8;
                    data[dst..dst + n].copy_from_slice(&block[row * 8..row * 8 + n]);
                }
            }
        }
        let _ = pw;
        planes.push(YuvPlane {
            width: cw,
            height: ch,
            data,
        });
    }
    Ok(YuvImage {
        width: frame.width,
        height: frame.height,
        subsampling,
        planes,
        progressive: frame.progressive,
        end: parsed.end,
    })
}

/// EXIF orientation out of an APP1 segment (the `Exif\0\0` header, then TIFF).
fn exif_orientation(seg: &[u8]) -> Option<u8> {
    crate::exif::tiff_orientation(seg.strip_prefix(b"Exif\0\0")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_idct_basis_matches_the_cosine_it_stands_for() {
        for v in 0..8usize {
            for x in 0..8usize {
                let c = if v == 0 {
                    1.0 / std::f64::consts::SQRT_2
                } else {
                    1.0
                };
                let want = 0.5
                    * c
                    * ((2 * x + 1) as f64 * v as f64 * std::f64::consts::PI / 16.0).cos()
                    * 8192.0;
                let got = f64::from(IDCT_BASIS[v][x]);
                assert!((want - got).abs() <= 0.5, "v{v} x{x}: {want} vs {got}");
            }
        }
    }

    #[test]
    fn the_dc_only_shortcut_matches_the_full_transform() {
        // The full transform, as idct_block computes it with no shortcut.
        fn full(coeffs: &[i16], quant: &[u16; 64]) -> [u8; 64] {
            let mut tmp = [0i64; 64];
            for x in 0..8 {
                for y in 0..8 {
                    let mut sum = 0i64;
                    for v in 0..8 {
                        let c = i64::from(coeffs[v * 8 + x]) * i64::from(quant[v * 8 + x]);
                        sum += c * i64::from(IDCT_BASIS[v][y]);
                    }
                    tmp[y * 8 + x] = (sum + 16) >> 5;
                }
            }
            let mut out = [0u8; 64];
            for y in 0..8 {
                for x in 0..8 {
                    let mut sum = 0i64;
                    for u in 0..8 {
                        sum += tmp[y * 8 + u] * i64::from(IDCT_BASIS[u][x]);
                    }
                    let value = ((sum + (1 << 20)) >> 21) + 128;
                    out[y * 8 + x] = value.clamp(0, 255) as u8;
                }
            }
            out
        }
        let quant = [3u16; 64];
        for dc in [-32768i32, -2048, -7, -1, 0, 1, 5, 64, 1000, 32767] {
            let mut coeffs = [0i16; 64];
            coeffs[0] = dc as i16;
            let mut fast = [0u8; 64];
            idct_block(&coeffs, &quant, &mut fast);
            assert_eq!(fast, full(&coeffs, &quant), "DC {dc}");
        }
    }

    #[test]
    fn a_flat_block_inverts_to_its_dc_level() {
        let mut coeffs = [0i16; 64];
        coeffs[0] = 8; // DC of 8 * quant 16 = 128 -> mid grey plus level shift
        let quant = [16u16; 64];
        let mut out = [0u8; 64];
        idct_block(&coeffs, &quant, &mut out);
        for &s in &out {
            assert_eq!(s, 144, "flat block should be one level everywhere");
        }
    }

    #[test]
    fn ycbcr_neutral_chroma_is_grey() {
        for y in [0u8, 17, 128, 255] {
            assert_eq!(ycbcr_to_rgb(y, 128, 128), [y, y, y]);
        }
        // Full-range BT.601 red is Y=76 Cb=84 Cr=255 (JFIF has no head/foot
        // room), which must come back as saturated red and nothing else.
        let rgb = ycbcr_to_rgb(76, 84, 255);
        assert!(rgb[0] > 250 && rgb[1] < 6 && rgb[2] < 6, "{rgb:?}");
    }

    #[test]
    fn exif_orientation_is_read_from_either_byte_order() {
        // Little-endian TIFF, one IFD entry: tag 0x0112, SHORT, count 1, value 6.
        let mut seg = b"Exif\0\0II".to_vec();
        seg.extend_from_slice(&42u16.to_le_bytes());
        seg.extend_from_slice(&8u32.to_le_bytes());
        seg.extend_from_slice(&1u16.to_le_bytes());
        seg.extend_from_slice(&0x0112u16.to_le_bytes());
        seg.extend_from_slice(&3u16.to_le_bytes());
        seg.extend_from_slice(&1u32.to_le_bytes());
        seg.extend_from_slice(&6u16.to_le_bytes());
        seg.extend_from_slice(&0u16.to_le_bytes());
        assert_eq!(exif_orientation(&seg), Some(6));
        assert_eq!(exif_orientation(b"Exif\0\0XX"), None);
    }
}
