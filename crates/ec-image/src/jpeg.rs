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
use crate::{Image, ImageFormat, Info, Limits, Metadata, Pixels};
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
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8], at: usize) -> BitReader<'a> {
        BitReader {
            data,
            at,
            bits: 0,
            count: 0,
            hit_marker: false,
        }
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
                        0
                    }
                },
                Some(&b) => {
                    self.at += 1;
                    b
                }
                None => {
                    self.hit_marker = true;
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

/// Dimensions from the frame header alone.
pub fn info(data: &[u8]) -> Result<Info> {
    let mut at = 2;
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
                return Ok(Info {
                    format: ImageFormat::Jpeg,
                    height: u32::from(u16::from_be_bytes([seg[1], seg[2]])),
                    width: u32::from(u16::from_be_bytes([seg[3], seg[4]])),
                });
            }
            0xd8 | 0xd9 => {}
            0xda => break,
            _ => {
                let len = be16(data, at)?;
                at += len.max(2);
            }
        }
    }
    Err(Error::corrupt("JPEG: no frame header"))
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

/// Decode a JPEG.
pub fn decode(data: &[u8], limits: Limits) -> Result<Image> {
    let mut quant = [[0u16; 64]; 4];
    let mut dc_tables: Vec<HuffTable> = vec![HuffTable::default(); 4];
    let mut ac_tables: Vec<HuffTable> = vec![HuffTable::default(); 4];
    let mut restart_interval = 0usize;
    let mut frame: Option<Frame> = None;
    let mut meta = Metadata::default();
    let mut adobe_transform: Option<u8> = None;

    let mut at = 2;
    while let Some(marker) = next_marker(data, &mut at) {
        match marker {
            0xd8 => continue,
            0xd9 => break,
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
                frame = Some(read_sof(seg, marker == 0xc2, limits)?);
            }
            0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => {
                return Err(Error::unsupported(
                    format!("JPEG SOF{}", marker & 0x0f),
                    "arithmetic-coded, lossless and hierarchical JPEG are a different process",
                ));
            }
            0xe1 => {
                if let Some(o) = exif_orientation(seg) {
                    meta.orientation = Some(o);
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
    let pixels = reconstruct(&frame, &quant, adobe_transform)?;
    Ok(Image {
        width: frame.width,
        height: frame.height,
        pixels,
        meta,
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
    for c in body.chunks_exact(3) {
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
        if cells * 2 > limits.max_alloc {
            return Err(Error::unsupported(
                "JPEG",
                "coefficient storage is past the allocation limit",
            ));
        }
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
    for c in spec.chunks_exact(2) {
        let index = frame
            .components
            .iter()
            .position(|f| f.id == c[0])
            .ok_or_else(|| Error::corrupt(format!("JPEG: scan names component {}", c[0])))?;
        in_scan.push((index, usize::from(c[1] >> 4), usize::from(c[1] & 0x0f)));
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

fn reconstruct(frame: &Frame, quant: &[[u16; 64]; 4], adobe: Option<u8>) -> Result<Pixels> {
    let (w, h) = (frame.width as usize, frame.height as usize);
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
            let cropped: Vec<u8> = (0..ch.min(ph))
                .flat_map(|y| plane[y * pw..y * pw + cw.min(pw)].to_vec())
                .collect();
            upsample(&cropped, cw.min(pw), ch.min(ph), w, h)
        })
        .collect();

    match frame.components.len() {
        1 => Ok(Pixels::L8(planes.into_iter().next().expect("one plane"))),
        3 => {
            // Component ids 'R','G','B' (or an Adobe transform of 0) mean the
            // samples are already RGB and must not be matrixed again.
            let ids: Vec<u8> = frame.components.iter().map(|c| c.id).collect();
            let already_rgb = adobe == Some(0) || ids == *b"RGB";
            let mut out = Vec::with_capacity(w * h * 3);
            for i in 0..w * h {
                if already_rgb {
                    out.extend_from_slice(&[planes[0][i], planes[1][i], planes[2][i]]);
                } else {
                    out.extend_from_slice(&ycbcr_to_rgb(planes[0][i], planes[1][i], planes[2][i]));
                }
            }
            Ok(Pixels::Rgb8(out))
        }
        n => Err(Error::unsupported(
            format!("{n}-component JPEG"),
            "no colour interpretation is defined for it",
        )),
    }
}

/// EXIF orientation (IFD0 tag 0x0112) out of an APP1 segment.
fn exif_orientation(seg: &[u8]) -> Option<u8> {
    let tiff = seg.strip_prefix(b"Exif\0\0")?;
    let little = match tiff.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b = tiff.get(at..at + 2)?;
        Some(if little {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b = tiff.get(at..at + 4)?;
        Some(if little {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd = u32_at(4)? as usize;
    let count = usize::from(u16_at(ifd)?);
    for i in 0..count {
        let entry = ifd + 2 + i * 12;
        if u16_at(entry)? == 0x0112 {
            let value = u16_at(entry + 8)?;
            return (1..=8).contains(&value).then_some(value as u8);
        }
    }
    None
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
